use std::ffi::CStr;
use std::ptr;

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use io_kit_sys::keys::kIOServicePlane;
use io_kit_sys::types::io_object_t;
use io_kit_sys::{
    IOIteratorIsValid, IOIteratorNext, IOObjectConformsTo, IOObjectRelease,
    IORegistryEntryCopyPath, IORegistryEntryCreateCFProperties, IORegistryEntryCreateIterator,
    IOServiceGetMatchingServices, IOServiceMatching, kIORegistryIterateParents,
    kIORegistryIterateRecursively,
};

use crate::devices::DeviceInfo;

struct IoObject(io_object_t);

impl Drop for IoObject {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns exactly one non-null IOKit reference.
        unsafe { IOObjectRelease(self.0) };
    }
}

impl IoObject {
    fn conforms_to(&self, class: &CStr) -> bool {
        // SAFETY: the object is live and CStr supplies a terminated class name.
        unsafe { IOObjectConformsTo(self.0, class.as_ptr()) != 0 }
    }

    fn properties(&self) -> Result<CFDictionary, String> {
        let mut properties = ptr::null_mut();
        // SAFETY: the output pointer is valid; the returned dictionary follows Create ownership.
        let status =
            unsafe { IORegistryEntryCreateCFProperties(self.0, &mut properties, ptr::null(), 0) };
        check_status("reading IOKit registry properties", status)?;
        if properties.is_null() {
            return Err("IOKit returned a null registry property dictionary".into());
        }
        // SAFETY: a successful CreateCFProperties returns an owned CFDictionary.
        Ok(unsafe { CFDictionary::wrap_under_create_rule(properties) })
    }

    fn registry_path(&self) -> Result<String, String> {
        // SAFETY: the entry is live and the plane is a static terminated string.
        let path = unsafe { IORegistryEntryCopyPath(self.0, kIOServicePlane) };
        if path.is_null() {
            return Err("IOKit could not obtain a physical disk's registry path".into());
        }
        // SAFETY: CopyPath returns an owned CFString.
        Ok(unsafe { CFString::wrap_under_create_rule(path) }.to_string())
    }
}

struct IoIterator(IoObject);

impl IoIterator {
    fn next(&self) -> Result<Option<IoObject>, String> {
        // SAFETY: the wrapper owns a live IOKit iterator.
        let object = unsafe { IOIteratorNext(self.0.0) };
        if object != 0 {
            return Ok(Some(IoObject(object)));
        }
        // A registry mutation can invalidate an iterator, which is not an empty result.
        // SAFETY: the iterator remains live even after it is exhausted.
        if unsafe { IOIteratorIsValid(self.0.0) } == 0 {
            Err("IOKit registry changed during enumeration; retry device discovery".into())
        } else {
            Ok(None)
        }
    }

    fn ancestors(entry: &IoObject) -> Result<Self, String> {
        let mut iterator = 0;
        // SAFETY: entry is live; iterator is a valid output pointer.
        let status = unsafe {
            IORegistryEntryCreateIterator(
                entry.0,
                kIOServicePlane,
                kIORegistryIterateParents | kIORegistryIterateRecursively,
                &mut iterator,
            )
        };
        check_status("creating IOKit ancestry iterator", status)?;
        if iterator == 0 {
            return Err("IOKit returned a null ancestry iterator".into());
        }
        Ok(Self(IoObject(iterator)))
    }
}

pub(super) fn enumerate() -> Result<Vec<DeviceInfo>, String> {
    // SAFETY: the class name is static and terminated. GetMatchingServices consumes
    // this matching dictionary, even if the call fails, so it must not be wrapped.
    let matching = unsafe { IOServiceMatching(c"IOMedia".as_ptr()) };
    if matching.is_null() {
        return Err("IOKit could not allocate an IOMedia matching dictionary".into());
    }
    let mut iterator = 0;
    // SAFETY: matching is a newly created dictionary and iterator is a valid output.
    // Port 0 selects the default IOKit main port without owning a Mach port.
    let status = unsafe { IOServiceGetMatchingServices(0, matching, &mut iterator) };
    check_status("enumerating IOKit media", status)?;
    if iterator == 0 {
        return Err("IOKit returned a null media iterator".into());
    }
    let iterator = IoIterator(IoObject(iterator));
    let mut devices = Vec::new();
    while let Some(media) = iterator.next()? {
        if let Some(device) = inspect_media(&media)? {
            devices.push(device);
        }
    }
    devices.sort_by(|left, right| left.path.cmp(&right.path));
    devices.dedup_by(|left, right| left.path == right.path);
    Ok(devices)
}

fn inspect_media(media: &IoObject) -> Result<Option<DeviceInfo>, String> {
    let properties = media.properties()?;
    match bool_property(&properties, "Whole") {
        Some(false) => return Ok(None),
        Some(true) => {}
        None => return Err("IOMedia is missing a boolean Whole property".into()),
    }

    let mut ancestry = Ancestry::default();
    ancestry.observe(media, &properties);
    let ancestors = IoIterator::ancestors(media)?;
    while let Some(parent) = ancestors.next()? {
        // A physical whole disk never has another IOMedia above it. In particular,
        // APFS synthesized disks can inherit USB ancestry through their backing store.
        ancestry.backed_by_media |= parent.conforms_to(c"IOMedia");
        let parent_properties = parent.properties()?;
        ancestry.observe(&parent, &parent_properties);
    }
    if !ancestry.is_usb_physical_disk() {
        return Ok(None);
    }

    let bsd_name = string_property(&properties, "BSD Name")
        .ok_or_else(|| "USB whole disk is missing its BSD Name".to_owned())?;
    if !is_whole_disk_name(&bsd_name) {
        return Err(format!(
            "USB whole disk has an invalid BSD Name: {bsd_name:?}"
        ));
    }
    let size_bytes = number_property(&properties, "Size")
        .and_then(|size| u64::try_from(size).ok())
        .ok_or_else(|| format!("{bsd_name}: missing or invalid IOMedia Size"))?;
    if size_bytes == 0 {
        return Ok(None);
    }
    let removable = bool_property(&properties, "Removable")
        .ok_or_else(|| format!("{bsd_name}: missing boolean IOMedia Removable property"))?;
    let writable = bool_property(&properties, "Writable")
        .ok_or_else(|| format!("{bsd_name}: missing boolean IOMedia Writable property"))?;
    let path = format!("/dev/{bsd_name}");
    let registry_path = media.registry_path()?;
    let serial = ancestry.disk_serial.or(ancestry.usb_serial);
    let name = ancestry.model.or(ancestry.usb_product).unwrap_or(bsd_name);
    Ok(Some(DeviceInfo {
        id: device_identity(serial.as_deref(), &registry_path),
        path,
        name,
        serial,
        size_bytes,
        removable,
        read_only: !writable,
    }))
}

#[derive(Default)]
struct Ancestry {
    usb: bool,
    block_device: bool,
    backed_by_media: bool,
    excluded: bool,
    internal: bool,
    seen_usb_device: bool,
    model: Option<String>,
    disk_serial: Option<String>,
    usb_serial: Option<String>,
    usb_product: Option<String>,
}

impl Ancestry {
    fn observe(&mut self, entry: &IoObject, properties: &CFDictionary) {
        self.block_device |= entry.conforms_to(c"IOBlockStorageDevice");
        self.excluded |= [
            c"IOCDMedia",
            c"IODVDMedia",
            c"IOBDMedia",
            c"IOCDBlockStorageDevice",
            c"IODiskImageBlockStorageDevice",
            c"IOHDIXHDDrive",
            c"AppleAPFSContainer",
            c"AppleAPFSVolume",
        ]
        .iter()
        .any(|class| entry.conforms_to(class));

        // These dictionaries live on the storage device, not necessarily IOMedia.
        let protocol = dictionary_property(properties, "Protocol Characteristics");
        for dictionary in std::iter::once(properties).chain(protocol.as_ref()) {
            if let Some(interconnect) = string_property(dictionary, "Physical Interconnect") {
                self.usb |= interconnect.eq_ignore_ascii_case("USB");
                self.excluded |= matches!(
                    interconnect.to_ascii_lowercase().as_str(),
                    "virtual interface" | "disk image" | "virtual"
                );
            }
            self.internal |= string_property(dictionary, "Physical Interconnect Location")
                .is_some_and(|location| location.eq_ignore_ascii_case("Internal"));
        }

        let usb_device = entry.conforms_to(c"IOUSBHostDevice") || entry.conforms_to(c"IOUSBDevice");
        if !usb_device && !self.seen_usb_device {
            let characteristics = dictionary_property(properties, "Device Characteristics");
            for dictionary in std::iter::once(properties).chain(characteristics.as_ref()) {
                if self.model.is_none() {
                    self.model = string_property(dictionary, "Product Name")
                        .or_else(|| string_property(dictionary, "Model"));
                }
                if self.disk_serial.is_none() {
                    self.disk_serial = string_property(dictionary, "Serial Number")
                        .or_else(|| string_property(dictionary, "SCSI Serial Number"));
                }
            }
        }
        self.usb |= usb_device;
        // Do not mistake an upstream hub's serial/product for the drive's identity.
        if usb_device && !self.seen_usb_device {
            self.seen_usb_device = true;
            self.usb_serial = string_property(properties, "USB Serial Number");
            self.usb_product = string_property(properties, "USB Product Name");
        }
    }

    fn is_usb_physical_disk(&self) -> bool {
        self.usb && self.block_device && !self.backed_by_media && !self.excluded && !self.internal
    }
}

fn property(dictionary: &CFDictionary, key: &str) -> Option<CFType> {
    let key = CFString::new(key);
    let value = dictionary.find(key.as_CFTypeRef())?;
    // SAFETY: CF property dictionaries contain CFType values. The dictionary owns
    // this borrowed value; Get ownership retains it before the dictionary can drop.
    Some(unsafe { CFType::wrap_under_get_rule(*value) })
}

fn dictionary_property(dictionary: &CFDictionary, key: &str) -> Option<CFDictionary> {
    property(dictionary, key)?.downcast::<CFDictionary>()
}

fn string_property(dictionary: &CFDictionary, key: &str) -> Option<String> {
    let value = property(dictionary, key)?
        .downcast::<CFString>()?
        .to_string();
    let value = value.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    (!value.is_empty()).then(|| value.to_owned())
}

fn bool_property(dictionary: &CFDictionary, key: &str) -> Option<bool> {
    property(dictionary, key)?
        .downcast::<CFBoolean>()
        .map(bool::from)
}

fn number_property(dictionary: &CFDictionary, key: &str) -> Option<i64> {
    property(dictionary, key)?.downcast::<CFNumber>()?.to_i64()
}

fn is_whole_disk_name(name: &str) -> bool {
    name.strip_prefix("disk").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn device_identity(serial: Option<&str>, registry_path: &str) -> String {
    // Registry topology distinguishes devices with duplicate/missing bridge serials
    // and multiple LUNs without relying on the unstable BSD disk number.
    match serial {
        Some(serial) => format!("macos-usb:{}:{serial}:{registry_path}", serial.len()),
        None => format!("macos-usb:{registry_path}"),
    }
}

fn check_status(operation: &str, status: i32) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!(
            "{operation} failed (IOKit status 0x{:08x})",
            status as u32
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_whole_bsd_disk_names() {
        for name in ["disk0", "disk12", "disk999"] {
            assert!(is_whole_disk_name(name));
        }
        for name in [
            "disk",
            "disk1s1",
            "/dev/disk1",
            "rdisk1",
            "disk-1",
            "disk1/other",
        ] {
            assert!(!is_whole_disk_name(name));
        }
    }

    #[test]
    fn usb_ancestry_alone_does_not_make_a_physical_disk() {
        let mut ancestry = Ancestry {
            usb: true,
            ..Default::default()
        };
        assert!(!ancestry.is_usb_physical_disk());
        ancestry.block_device = true;
        assert!(ancestry.is_usb_physical_disk());
        ancestry.backed_by_media = true;
        assert!(!ancestry.is_usb_physical_disk());
        ancestry.backed_by_media = false;
        ancestry.excluded = true;
        assert!(!ancestry.is_usb_physical_disk());
        ancestry.excluded = false;
        ancestry.internal = true;
        assert!(!ancestry.is_usb_physical_disk());
    }

    #[test]
    fn identity_distinguishes_duplicate_serials_and_missing_serials() {
        assert_ne!(
            device_identity(Some("123"), "a"),
            device_identity(Some("123"), "b")
        );
        assert_ne!(device_identity(None, "a"), device_identity(None, "b"));
        assert_eq!(
            device_identity(Some("123"), "a"),
            device_identity(Some("123"), "a")
        );
    }

    #[test]
    fn reads_typed_registry_values_without_coercion() {
        let dictionary = CFDictionary::from_CFType_pairs(&[
            (
                CFString::new("Whole").as_CFType(),
                CFBoolean::true_value().as_CFType(),
            ),
            (
                CFString::new("Size").as_CFType(),
                CFNumber::from(4096_i64).as_CFType(),
            ),
            (
                CFString::new("Model").as_CFType(),
                CFString::new("  USB SSD\0").as_CFType(),
            ),
        ])
        .into_untyped();
        assert_eq!(bool_property(&dictionary, "Whole"), Some(true));
        assert_eq!(number_property(&dictionary, "Size"), Some(4096));
        assert_eq!(
            string_property(&dictionary, "Model").as_deref(),
            Some("USB SSD")
        );
        assert_eq!(bool_property(&dictionary, "Size"), None);
        assert_eq!(string_property(&dictionary, "Missing"), None);
    }
}
