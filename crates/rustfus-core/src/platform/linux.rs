use std::fs;
use std::io;
use std::path::Path;

use crate::devices::DeviceInfo;

#[cfg(target_os = "linux")]
pub(super) fn enumerate() -> Result<Vec<DeviceInfo>, String> {
    enumerate_at(Path::new("/sys/class/block"))
}

#[cfg(any(target_os = "linux", test))]
fn enumerate_at(root: &Path) -> Result<Vec<DeviceInfo>, String> {
    let entries =
        fs::read_dir(root).map_err(|error| format!("Cannot read {}: {error}", root.display()))?;
    let mut devices = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("Cannot enumerate block devices: {error}"))?;
        let path = entry.path();
        match inspect(&path) {
            Ok(Some(device)) => devices.push(device),
            Ok(None) => {}
            // Unplugging during a scan is normal; never retain a vanished device.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot inspect {}: {error}", path.display())),
        }
    }
    Ok(devices)
}

fn inspect(path: &Path) -> io::Result<Option<DeviceInfo>> {
    if path.join("partition").exists() {
        return Ok(None);
    }
    let canonical = path.canonicalize()?;
    let Some(usb) = canonical.ancestors().find(|ancestor| {
        ancestor.join("idVendor").is_file() && ancestor.join("idProduct").is_file()
    }) else {
        return Ok(None);
    };
    // USB optical drives are not flashable disk targets (SCSI type 0 = direct access).
    if read_number(&path.join("device/type"))? != 0 {
        return Ok(None);
    }
    // The Linux sysfs size ABI is always in 512-byte sectors, including 4K disks.
    let size_bytes = read_number(&path.join("size"))?
        .checked_mul(512)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "disk size overflow"))?;
    if size_bytes == 0 {
        return Ok(None);
    }
    let removable = read_flag(&path.join("removable"))?;
    let read_only = read_flag(&path.join("ro"))?;
    let serial = text(&path.join("device/serial")).or_else(|| text(&usb.join("serial")));
    let kernel_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing disk name"))?
        .to_string_lossy();
    let name = text(&path.join("device/model"))
        .or_else(|| text(&usb.join("product")))
        .unwrap_or_else(|| kernel_name.to_string());
    let topology = canonical.to_string_lossy();
    Ok(Some(DeviceInfo {
        id: format!("linux-usb:{}:{topology}", serial.as_deref().unwrap_or("")),
        path: format!("/dev/{kernel_name}"),
        name,
        serial,
        size_bytes,
        removable,
        read_only,
    }))
}

fn text(path: &Path) -> Option<String> {
    let value = fs::read_to_string(path).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn read_number(path: &Path) -> io::Result<u64> {
    fs::read_to_string(path)?.trim().parse().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid {}: {error}", path.display()),
        )
    })
}

fn read_flag(path: &Path) -> io::Result<bool> {
    match read_number(path)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid sysfs boolean",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(root: &Path, usb: bool, removable: bool, scsi_type: &str) -> std::path::PathBuf {
        let parent = root.join(if usb { "usb-device" } else { "internal-device" });
        let disk = parent.join("block/sdb");
        fs::create_dir_all(disk.join("device")).unwrap();
        if usb {
            fs::write(parent.join("idVendor"), "1234").unwrap();
            fs::write(parent.join("idProduct"), "5678").unwrap();
            fs::write(parent.join("serial"), "USB-123").unwrap();
        }
        for (file, value) in [
            ("device/type", scsi_type),
            ("device/model", "  Test USB disk\n"),
            ("size", "15625000"),
            ("removable", if removable { "1" } else { "0" }),
            ("ro", "0"),
        ] {
            fs::write(disk.join(file), value).unwrap();
        }
        disk
    }

    #[test]
    fn discovers_unmounted_usb_and_fixed_usb_disks() {
        let dir = tempfile::tempdir().unwrap();
        let path = disk(dir.path(), true, false, "0");
        let device = inspect(&path).unwrap().unwrap();
        assert_eq!(device.name, "Test USB disk");
        assert_eq!(device.path, "/dev/sdb");
        assert_eq!(device.size_bytes, 8_000_000_000);
        assert_eq!(device.serial.as_deref(), Some("USB-123"));
        assert!(!device.removable);
    }

    #[test]
    fn excludes_internal_disks_partitions_and_optical_media() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            inspect(&disk(dir.path(), false, true, "0"))
                .unwrap()
                .is_none()
        );
        let path = disk(dir.path(), true, true, "5");
        assert!(inspect(&path).unwrap().is_none());
        fs::write(path.join("device/type"), "0").unwrap();
        fs::write(path.join("partition"), "1").unwrap();
        assert!(inspect(&path).unwrap().is_none());
    }

    #[test]
    fn rejects_malformed_geometry_instead_of_inventing_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let path = disk(dir.path(), true, true, "0");
        fs::write(path.join("size"), u64::MAX.to_string()).unwrap();
        assert!(inspect(&path).is_err());
        fs::write(path.join("size"), "not a number").unwrap();
        assert!(inspect(&path).is_err());
    }

    #[test]
    fn missing_registry_is_an_error_not_an_empty_device_list() {
        assert!(enumerate_at(Path::new("/no-such-rustfus-registry")).is_err());
    }
}
