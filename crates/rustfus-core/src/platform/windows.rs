use std::collections::HashSet;
use std::ffi::c_void;
use std::fmt;
use std::mem::{offset_of, size_of};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces,
    SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_DEV_NOT_EXIST, ERROR_DEVICE_NOT_CONNECTED,
    ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_FUNCTION, ERROR_MORE_DATA,
    ERROR_NO_MEDIA_IN_DRIVE, ERROR_NO_MORE_ITEMS, ERROR_NOT_READY, ERROR_NOT_SUPPORTED,
    ERROR_PATH_NOT_FOUND, ERROR_WRITE_PROTECT, GENERIC_READ, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    BusTypeUsb, CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    DISK_ATTRIBUTE_READ_ONLY, DISK_GEOMETRY_EX, GET_DISK_ATTRIBUTES, GET_LENGTH_INFORMATION,
    GUID_DEVINTERFACE_DISK, IOCTL_DISK_GET_DISK_ATTRIBUTES, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
    IOCTL_DISK_GET_LENGTH_INFO, IOCTL_DISK_IS_WRITABLE, IOCTL_STORAGE_GET_DEVICE_NUMBER,
    IOCTL_STORAGE_QUERY_PROPERTY, IOCTL_STORAGE_READ_CAPACITY, PropertyStandardQuery,
    STORAGE_DESCRIPTOR_HEADER, STORAGE_DEVICE_DESCRIPTOR, STORAGE_DEVICE_NUMBER,
    STORAGE_PROPERTY_QUERY, STORAGE_READ_CAPACITY, StorageDeviceProperty,
};

const MAX_BUFFER: usize = 1024 * 1024;
// FILE_DEVICE_DISK from winioctl.h; unlike FILE_DEVICE_DISK_FILE_SYSTEM (8), this is 7.
const DISK_DEVICE_TYPE: u32 = 7;

#[derive(Debug)]
enum QueryError {
    Windows { operation: &'static str, code: u32 },
    Invalid(String),
    NoMedia,
}

fn disk_name(name: Option<String>, disk_number: u32) -> String {
    name.unwrap_or_else(|| format!("PhysicalDrive{disk_number}"))
}

impl QueryError {
    fn allows_capacity_fallback(&self) -> bool {
        matches!(
            self,
            Self::Windows {
                code: ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED | ERROR_ACCESS_DENIED,
                ..
            }
        )
    }

    fn is_absent(&self) -> bool {
        matches!(
            self,
            Self::NoMedia
                | Self::Windows {
                    code: ERROR_NO_MEDIA_IN_DRIVE
                        | ERROR_NOT_READY
                        | ERROR_DEVICE_NOT_CONNECTED
                        | ERROR_FILE_NOT_FOUND
                        | ERROR_PATH_NOT_FOUND
                        | ERROR_DEV_NOT_EXIST,
                    ..
                }
        )
    }
}

impl From<String> for QueryError {
    fn from(message: String) -> Self {
        Self::Invalid(message)
    }
}

impl From<&str> for QueryError {
    fn from(message: &str) -> Self {
        Self::Invalid(message.into())
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Windows { operation, code } => write!(
                f,
                "{operation}: {} (Windows error {code})",
                std::io::Error::from_raw_os_error(*code as i32)
            ),
            Self::Invalid(message) => f.write_str(message),
            Self::NoMedia => f.write_str("Disk reports no media (zero capacity)"),
        }
    }
}

struct DeviceSet(HDEVINFO);

impl Drop for DeviceSet {
    fn drop(&mut self) {
        // SAFETY: this owns a successful SetupDiGetClassDevsW result, closed exactly once.
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

struct DiskHandle(HANDLE);

impl Drop for DiskHandle {
    fn drop(&mut self) {
        // SAFETY: this owns a successful CreateFileW result, closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

// Vec<u8> does not promise the alignment required by Win32's output structures.
struct Buffer {
    words: Vec<u64>,
    len: usize,
}

impl Buffer {
    fn new(len: usize) -> Result<Self, QueryError> {
        if len == 0 || len > MAX_BUFFER {
            return Err(format!("Invalid Windows storage buffer size: {len}").into());
        }
        Ok(Self {
            words: vec![0; len.div_ceil(size_of::<u64>())],
            len,
        })
    }

    fn as_mut_ptr(&mut self) -> *mut c_void {
        self.words.as_mut_ptr().cast()
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: all words are initialized, u8 has alignment 1, and len is within the allocation.
        unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.len) }
    }
}

fn win_error(operation: &'static str, code: u32) -> QueryError {
    QueryError::Windows { operation, code }
}

fn open_disk(path: &[u16], access: u32) -> Result<DiskHandle, QueryError> {
    // SAFETY: callers supply a NUL-terminated UTF-16 path; no security attributes or template.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        // SAFETY: GetLastError immediately follows the failing Win32 call.
        return Err(win_error("Opening disk interface", unsafe {
            GetLastError()
        }));
    }
    Ok(DiskHandle(handle))
}

fn ioctl(
    handle: &DiskHandle,
    code: u32,
    input: Option<&STORAGE_PROPERTY_QUERY>,
    output: Option<&mut Buffer>,
) -> Result<usize, u32> {
    let (input_ptr, input_len) = input.map_or((null(), 0), |value| {
        (
            (value as *const STORAGE_PROPERTY_QUERY).cast(),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
        )
    });
    let (output_ptr, output_len) = output.map_or((null_mut(), 0), |buffer| {
        (buffer.as_mut_ptr(), buffer.len as u32)
    });
    let mut returned = 0;
    // SAFETY: pointers cover initialized input and writable, aligned output for the stated sizes.
    // Handles are synchronous and owned for the duration; no OVERLAPPED pointer is supplied.
    let ok = unsafe {
        DeviceIoControl(
            handle.0,
            code,
            input_ptr,
            input_len,
            output_ptr,
            output_len,
            &mut returned,
            null_mut(),
        )
    };
    if ok == 0 {
        // SAFETY: queried immediately after the failed call.
        Err(unsafe { GetLastError() })
    } else if returned > output_len {
        // A successful kernel response must never claim more bytes than its output buffer.
        Err(ERROR_INSUFFICIENT_BUFFER)
    } else {
        Ok(returned as usize)
    }
}

fn query_buffer(handle: &DiskHandle, code: u32, initial: usize) -> Result<Buffer, QueryError> {
    let mut size = initial;
    loop {
        let mut buffer = Buffer::new(size)?;
        match ioctl(handle, code, None, Some(&mut buffer)) {
            Ok(returned) => {
                buffer.len = returned;
                return Ok(buffer);
            }
            Err(ERROR_INSUFFICIENT_BUFFER | ERROR_MORE_DATA) if size < MAX_BUFFER => {
                size = (size * 2).min(MAX_BUFFER);
            }
            Err(code) => return Err(win_error("Querying disk information", code)),
        }
    }
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, QueryError> {
    let field = bytes
        .get(offset..offset + 4)
        .ok_or("Truncated Windows storage response")?;
    Ok(u32::from_le_bytes(field.try_into().unwrap()))
}

fn i64_at(bytes: &[u8], offset: usize) -> Result<i64, QueryError> {
    let field = bytes
        .get(offset..offset + 8)
        .ok_or("Truncated Windows storage response")?;
    Ok(i64::from_le_bytes(field.try_into().unwrap()))
}

fn positive_length(bytes: &[u8], offset: usize) -> Result<u64, QueryError> {
    let length = i64_at(bytes, offset)?;
    if length == 0 {
        return Err(QueryError::NoMedia);
    }
    if length < 0 {
        return Err("Disk reports a negative capacity".into());
    }
    Ok(length as u64)
}

fn descriptor_string(bytes: &[u8], offset: u32) -> Result<Option<String>, QueryError> {
    if offset == 0 {
        return Ok(None);
    }
    let offset = offset as usize;
    if offset < offset_of!(STORAGE_DEVICE_DESCRIPTOR, RawDeviceProperties) {
        return Err("Storage descriptor string points into its header".into());
    }
    let tail = bytes
        .get(offset..)
        .ok_or("Storage descriptor string is out of bounds")?;
    let end = tail
        .iter()
        .position(|&byte| byte == 0)
        .ok_or("Unterminated storage descriptor string")?;
    let value = String::from_utf8_lossy(&tail[..end]).trim().to_owned();
    Ok((!value.is_empty()).then_some(value))
}

struct Descriptor {
    usb: bool,
    removable: bool,
    name: Option<String>,
    serial: Option<String>,
}

fn parse_descriptor(bytes: &[u8]) -> Result<Descriptor, QueryError> {
    let declared = u32_at(bytes, offset_of!(STORAGE_DEVICE_DESCRIPTOR, Size))? as usize;
    if declared < offset_of!(STORAGE_DEVICE_DESCRIPTOR, RawDeviceProperties)
        || declared > bytes.len()
    {
        return Err("Invalid or truncated storage device descriptor".into());
    }
    let bytes = &bytes[..declared];
    let usb = u32_at(bytes, offset_of!(STORAGE_DEVICE_DESCRIPTOR, BusType))? == BusTypeUsb as u32;
    // A disk interface can include optical/other storage. SCSI peripheral type 0 is direct-access.
    let direct_access = bytes[offset_of!(STORAGE_DEVICE_DESCRIPTOR, DeviceType)] == 0;
    if !usb || !direct_access {
        return Ok(Descriptor {
            usb: false,
            removable: false,
            name: None,
            serial: None,
        });
    }
    let removable = match bytes[offset_of!(STORAGE_DEVICE_DESCRIPTOR, RemovableMedia)] {
        0 => false,
        1 => true,
        _ => return Err("Invalid RemovableMedia value in storage descriptor".into()),
    };
    let vendor = descriptor_string(
        bytes,
        u32_at(bytes, offset_of!(STORAGE_DEVICE_DESCRIPTOR, VendorIdOffset))?,
    )?;
    let product = descriptor_string(
        bytes,
        u32_at(
            bytes,
            offset_of!(STORAGE_DEVICE_DESCRIPTOR, ProductIdOffset),
        )?,
    )?;
    let name = match (vendor, product) {
        (Some(vendor), Some(product))
            if !product.to_lowercase().starts_with(&vendor.to_lowercase()) =>
        {
            Some(format!("{vendor} {product}"))
        }
        (_, Some(product)) => Some(product),
        (Some(vendor), None) => Some(vendor),
        (None, None) => None,
    };
    let serial = descriptor_string(
        bytes,
        u32_at(
            bytes,
            offset_of!(STORAGE_DEVICE_DESCRIPTOR, SerialNumberOffset),
        )?,
    )?;
    Ok(Descriptor {
        usb: true,
        removable,
        name,
        serial,
    })
}

fn storage_descriptor(handle: &DiskHandle) -> Result<Descriptor, QueryError> {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        ..Default::default()
    };
    let mut header = Buffer::new(size_of::<STORAGE_DESCRIPTOR_HEADER>())?;
    let returned = ioctl(
        handle,
        IOCTL_STORAGE_QUERY_PROPERTY,
        Some(&query),
        Some(&mut header),
    )
    .map_err(|code| win_error("Reading storage descriptor header", code))?;
    let mut size = u32_at(
        &header.bytes()[..returned],
        offset_of!(STORAGE_DESCRIPTOR_HEADER, Size),
    )? as usize;
    for _ in 0..4 {
        let mut buffer = Buffer::new(size)?;
        match ioctl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query),
            Some(&mut buffer),
        ) {
            Ok(returned) => {
                let declared = u32_at(
                    &buffer.bytes()[..returned],
                    offset_of!(STORAGE_DEVICE_DESCRIPTOR, Size),
                )? as usize;
                if declared > size {
                    size = declared;
                    continue;
                }
                return parse_descriptor(&buffer.bytes()[..returned]);
            }
            Err(ERROR_INSUFFICIENT_BUFFER | ERROR_MORE_DATA) => size = size.saturating_mul(2),
            Err(code) => return Err(win_error("Reading storage device descriptor", code)),
        }
    }
    Err("Storage descriptor kept changing size".into())
}

fn capacity(handle: &DiskHandle, interface_path: &[u16]) -> Result<u64, QueryError> {
    // GEOMETRY_EX uses FILE_ANY_ACCESS, unlike LENGTH_INFO and READ_CAPACITY (FILE_READ_ACCESS).
    // DiskSize is exact, not the rounded CHS product of the older GEOMETRY ioctl.
    let geometry = query_buffer(handle, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, 1024)
        .and_then(|buffer| positive_length(buffer.bytes(), offset_of!(DISK_GEOMETRY_EX, DiskSize)));
    match geometry {
        Ok(length) => Ok(length),
        Err(error) if !error.allows_capacity_fallback() => Err(error),
        Err(_) => {
            let readable = open_disk(interface_path, GENERIC_READ)?;
            let length = query_buffer(
                &readable,
                IOCTL_DISK_GET_LENGTH_INFO,
                size_of::<GET_LENGTH_INFORMATION>(),
            )
            .and_then(|buffer| {
                positive_length(buffer.bytes(), offset_of!(GET_LENGTH_INFORMATION, Length))
            });
            match length {
                Ok(length) => Ok(length),
                Err(error) if !error.allows_capacity_fallback() => Err(error),
                Err(_) => query_buffer(
                    &readable,
                    IOCTL_STORAGE_READ_CAPACITY,
                    size_of::<STORAGE_READ_CAPACITY>(),
                )
                .and_then(|buffer| {
                    positive_length(
                        buffer.bytes(),
                        offset_of!(STORAGE_READ_CAPACITY, DiskLength),
                    )
                }),
            }
        }
    }
}

fn read_only(handle: &DiskHandle) -> Result<bool, QueryError> {
    // Check Windows' software read-only flag as well as hardware write protection. Neither ioctl writes.
    let mut attributes = Buffer::new(size_of::<GET_DISK_ATTRIBUTES>())?;
    let attribute_result = ioctl(
        handle,
        IOCTL_DISK_GET_DISK_ATTRIBUTES,
        None,
        Some(&mut attributes),
    );
    let software_read_only = match attribute_result {
        Ok(returned) => {
            let value = i64_at(
                &attributes.bytes()[..returned],
                offset_of!(GET_DISK_ATTRIBUTES, Attributes),
            )? as u64;
            value & DISK_ATTRIBUTE_READ_ONLY != 0
        }
        Err(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED) => false,
        Err(code) => return Err(win_error("Querying disk read-only attributes", code)),
    };
    if software_read_only {
        return Ok(true);
    }
    writable_result(ioctl(handle, IOCTL_DISK_IS_WRITABLE, None, None))
}

fn writable_result(result: Result<usize, u32>) -> Result<bool, QueryError> {
    match result {
        Ok(_) => Ok(false),
        Err(ERROR_WRITE_PROTECT) => Ok(true),
        Err(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED) => Ok(true),
        Err(code) => Err(win_error(
            "Querying disk write protection (without writing)",
            code,
        )),
    }
}

fn interface_path(
    set: &DeviceSet,
    interface: &SP_DEVICE_INTERFACE_DATA,
) -> Result<Vec<u16>, QueryError> {
    let mut required = 0;
    // SAFETY: valid set/interface, null output requests the required allocation size.
    let ok = unsafe {
        SetupDiGetDeviceInterfaceDetailW(set.0, interface, null_mut(), 0, &mut required, null_mut())
    };
    // SAFETY: queried immediately after the sizing call.
    let error = unsafe { GetLastError() };
    if ok != 0 || error != ERROR_INSUFFICIENT_BUFFER {
        return Err(win_error("Sizing disk interface detail", error));
    }
    let mut buffer = Buffer::new(required as usize)?;
    if buffer.len < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() {
        return Err("Disk interface detail is smaller than its header".into());
    }
    let detail = buffer
        .as_mut_ptr()
        .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
    // SAFETY: allocation is at least size_of(detail) and u64-aligned; cbSize must be the native
    // structure size (8 on Win64, 6 on Win32), not the variable allocation size.
    unsafe { (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 };
    // SAFETY: aligned detail buffer covers required bytes; interface belongs to the live set.
    let ok = unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            set.0,
            interface,
            detail,
            required,
            &mut required,
            null_mut(),
        )
    };
    if ok == 0 {
        // SAFETY: queried immediately after the failed call.
        return Err(win_error("Reading disk interface detail", unsafe {
            GetLastError()
        }));
    }
    let used = required as usize;
    if used > buffer.len {
        return Err("Disk interface detail exceeded its allocation".into());
    }
    let offset = offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
    let bytes = buffer
        .bytes()
        .get(offset..used)
        .ok_or("Truncated disk interface path")?;
    let mut path = Vec::new();
    for unit in bytes.chunks_exact(2) {
        let unit = u16::from_le_bytes([unit[0], unit[1]]);
        path.push(unit);
        if unit == 0 {
            return if path.len() > 1 {
                Ok(path)
            } else {
                Err("Empty disk interface path".into())
            };
        }
    }
    Err("Unterminated disk interface path".into())
}

pub(super) fn enumerate() -> Result<Vec<crate::devices::DeviceInfo>, String> {
    // SAFETY: class GUID is valid; null enumerator/window select all present disk interfaces.
    let raw_set = unsafe {
        SetupDiGetClassDevsW(
            &GUID_DEVINTERFACE_DISK,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if raw_set == -1 {
        // SAFETY: queried immediately after the failed call.
        return Err(win_error("Enumerating Windows disk interfaces", unsafe {
            GetLastError()
        })
        .to_string());
    }
    let set = DeviceSet(raw_set);
    let mut devices = Vec::new();
    let mut seen = HashSet::new();
    let mut index = 0u32;
    loop {
        let mut interface = SP_DEVICE_INTERFACE_DATA {
            cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: set is live and interface is correctly sized writable storage.
        let ok = unsafe {
            SetupDiEnumDeviceInterfaces(
                set.0,
                null(),
                &GUID_DEVINTERFACE_DISK,
                index,
                &mut interface,
            )
        };
        if ok == 0 {
            // SAFETY: queried immediately after the failed call.
            let code = unsafe { GetLastError() };
            if code == ERROR_NO_MORE_ITEMS {
                break;
            }
            if win_error("Enumerating disk interface", code).is_absent() {
                index = index
                    .checked_add(1)
                    .ok_or("Too many Windows disk interfaces")?;
                continue;
            }
            return Err(win_error("Enumerating disk interface", code).to_string());
        }
        index = index
            .checked_add(1)
            .ok_or("Too many Windows disk interfaces")?;
        let result: Result<Option<crate::devices::DeviceInfo>, QueryError> = (|| {
            let interface_path = interface_path(&set, &interface)?;
            // Zero desired access permits metadata queries without administrative read/write rights.
            let handle = open_disk(&interface_path, 0)?;
            let descriptor = storage_descriptor(&handle)?;
            if !descriptor.usb {
                return Ok(None);
            }
            let number = query_buffer(
                &handle,
                IOCTL_STORAGE_GET_DEVICE_NUMBER,
                size_of::<STORAGE_DEVICE_NUMBER>(),
            )?;
            if u32_at(
                number.bytes(),
                offset_of!(STORAGE_DEVICE_NUMBER, DeviceType),
            )? != DISK_DEVICE_TYPE
            {
                return Ok(None);
            }
            // GUID_DEVINTERFACE_DISK exposes whole disks, not volume or partition interfaces.
            let disk_number = u32_at(
                number.bytes(),
                offset_of!(STORAGE_DEVICE_NUMBER, DeviceNumber),
            )?;
            if disk_number == u32::MAX {
                return Err("USB disk has no physical disk number".into());
            }
            let path = format!(r"\\.\PhysicalDrive{disk_number}");
            let name = disk_name(descriptor.name, disk_number);
            let size_bytes = capacity(&handle, &interface_path)?;
            let read_only = read_only(&handle)?;
            let persistent_path =
                String::from_utf16_lossy(&interface_path[..interface_path.len() - 1])
                    .to_lowercase();
            // PhysicalDrive numbers can change across boots. Use the persistent interface instead.
            let id = match descriptor.serial.as_deref() {
                Some(serial) => format!("usb:{serial}:{persistent_path}"),
                None => format!("usb:{persistent_path}"),
            };
            Ok(Some(crate::devices::DeviceInfo {
                id,
                path,
                name,
                serial: descriptor.serial,
                size_bytes,
                removable: descriptor.removable,
                read_only,
            }))
        })();
        match result {
            Ok(Some(device)) if seen.insert(device.path.clone()) => devices.push(device),
            Ok(_) => {}
            Err(error) if error.is_absent() => {}
            Err(error) => return Err(format!("Disk interface {}: {error}", index - 1)),
        }
    }
    devices.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(bus: u32, removable: bool) -> Vec<u8> {
        let mut bytes = vec![0; size_of::<STORAGE_DEVICE_DESCRIPTOR>()];
        let size = bytes.len() as u32;
        bytes[offset_of!(STORAGE_DEVICE_DESCRIPTOR, Size)..][..4]
            .copy_from_slice(&size.to_le_bytes());
        bytes[offset_of!(STORAGE_DEVICE_DESCRIPTOR, BusType)..][..4]
            .copy_from_slice(&bus.to_le_bytes());
        bytes[offset_of!(STORAGE_DEVICE_DESCRIPTOR, RemovableMedia)] = u8::from(removable);
        bytes
    }

    #[test]
    fn fixed_usb_disks_are_included_but_internal_disks_are_not() {
        let usb = parse_descriptor(&descriptor(BusTypeUsb as u32, false)).unwrap();
        assert!(usb.usb);
        assert!(!usb.removable);
        assert!(!parse_descriptor(&descriptor(11, false)).unwrap().usb); // SATA
        assert!(!parse_descriptor(&descriptor(17, false)).unwrap().usb); // NVMe
    }

    #[test]
    fn descriptor_strings_require_valid_offsets_and_termination() {
        let mut bytes = descriptor(BusTypeUsb as u32, true);
        let offset = bytes.len() as u32;
        bytes.extend_from_slice(b"  Real model  \0");
        let size = bytes.len() as u32;
        bytes[offset_of!(STORAGE_DEVICE_DESCRIPTOR, Size)..][..4]
            .copy_from_slice(&size.to_le_bytes());
        bytes[offset_of!(STORAGE_DEVICE_DESCRIPTOR, ProductIdOffset)..][..4]
            .copy_from_slice(&offset.to_le_bytes());
        assert_eq!(
            parse_descriptor(&bytes).unwrap().name.as_deref(),
            Some("Real model")
        );
        assert!(descriptor_string(&bytes, 2).is_err());
        assert!(descriptor_string(&bytes, size + 1).is_err());
        bytes.pop();
        assert!(descriptor_string(&bytes, offset).is_err());
        assert!(parse_descriptor(&bytes).is_err());
    }

    #[test]
    fn descriptor_header_and_lengths_are_checked() {
        assert!(parse_descriptor(&[]).is_err());
        assert!(positive_length(&0i64.to_le_bytes(), 0).is_err());
        assert!(positive_length(&(-1i64).to_le_bytes(), 0).is_err());
        assert!(positive_length(&[1, 2], 0).is_err());
        assert_eq!(
            positive_length(&8_000_000_000_000i64.to_le_bytes(), 0).unwrap(),
            8_000_000_000_000
        );
    }

    #[test]
    fn absent_devices_are_classified_by_native_code_not_message() {
        assert_eq!(ERROR_NO_MEDIA_IN_DRIVE, 1112);
        for code in [
            ERROR_NO_MEDIA_IN_DRIVE,
            ERROR_NOT_READY,
            ERROR_DEVICE_NOT_CONNECTED,
            ERROR_FILE_NOT_FOUND,
            ERROR_PATH_NOT_FOUND,
            ERROR_DEV_NOT_EXIST,
        ] {
            for operation in [
                "Opening disk interface",
                "Reading storage device descriptor",
                "Querying disk information",
            ] {
                let error = win_error(operation, code);
                assert!(error.is_absent(), "{error}");
                assert!(!error.allows_capacity_fallback(), "{error}");
                assert!(
                    matches!(error, QueryError::Windows { code: actual, .. } if actual == code)
                );
            }
        }
        assert!(!QueryError::from("Windows error 1112: no media").is_absent());
        for code in [
            ERROR_ACCESS_DENIED,
            ERROR_INVALID_FUNCTION,
            ERROR_NOT_SUPPORTED,
            23,
        ] {
            assert!(!win_error("Querying disk information", code).is_absent());
        }
    }

    #[test]
    fn capacity_regression_fixtures_distinguish_empty_from_invalid_media() {
        for offset in [
            offset_of!(DISK_GEOMETRY_EX, DiskSize),
            offset_of!(GET_LENGTH_INFORMATION, Length),
            offset_of!(STORAGE_READ_CAPACITY, DiskLength),
        ] {
            for (length, absent) in [(0i64, true), (-1, false), (4096, false)] {
                let mut bytes = vec![0; offset + 8];
                bytes[offset..].copy_from_slice(&length.to_le_bytes());
                match positive_length(&bytes, offset) {
                    Ok(value) => assert_eq!(value, 4096),
                    Err(error) => {
                        assert_eq!(error.is_absent(), absent);
                        assert!(!error.allows_capacity_fallback());
                    }
                }
            }
            let error = positive_length(&[], offset).unwrap_err();
            assert!(!error.is_absent());
            assert!(!error.allows_capacity_fallback());
        }
        for code in [
            ERROR_ACCESS_DENIED,
            ERROR_INVALID_FUNCTION,
            ERROR_NOT_SUPPORTED,
        ] {
            assert!(win_error("Geometry", code).allows_capacity_fallback());
        }
        assert!(!win_error("Geometry", 23).allows_capacity_fallback()); // ERROR_CRC
    }

    #[test]
    fn unsupported_writability_is_conservatively_read_only() {
        assert!(!writable_result(Ok(0)).unwrap());
        for code in [
            ERROR_WRITE_PROTECT,
            ERROR_INVALID_FUNCTION,
            ERROR_NOT_SUPPORTED,
        ] {
            assert!(writable_result(Err(code)).unwrap());
        }
        assert!(
            writable_result(Err(ERROR_NOT_READY))
                .unwrap_err()
                .is_absent()
        );
        assert!(
            !writable_result(Err(ERROR_ACCESS_DENIED))
                .unwrap_err()
                .is_absent()
        );
    }

    #[test]
    fn descriptor_regression_fixtures_keep_usb_classification_and_missing_names() {
        for removable in [false, true] {
            let parsed = parse_descriptor(&descriptor(BusTypeUsb as u32, removable)).unwrap();
            assert!(parsed.usb);
            assert_eq!(parsed.removable, removable);
            assert_eq!(disk_name(parsed.name, 12), "PhysicalDrive12");
        }
        assert_eq!(disk_name(Some("Actual model".into()), 12), "Actual model");
        let mut optical = descriptor(BusTypeUsb as u32, true);
        optical[offset_of!(STORAGE_DEVICE_DESCRIPTOR, DeviceType)] = 5;
        assert!(!parse_descriptor(&optical).unwrap().usb);
        let mut malformed = descriptor(BusTypeUsb as u32, true);
        malformed[offset_of!(STORAGE_DEVICE_DESCRIPTOR, RemovableMedia)] = 2;
        let error = parse_descriptor(&malformed).err().unwrap();
        assert!(!error.is_absent());
    }
}
