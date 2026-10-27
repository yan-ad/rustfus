use std::collections::BTreeSet;
use std::fs::File;
use std::mem::{offset_of, size_of};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, ERROR_NO_MORE_FILES, GENERIC_READ, GENERIC_WRITE,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    BusTypeUsb, CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, FindFirstVolumeW, FindNextVolumeW,
    FindVolumeClose, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    DISK_EXTENT, DISK_GEOMETRY, DISK_GEOMETRY_EX, DRIVE_LAYOUT_INFORMATION_EX,
    FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, GPT_ATTRIBUTE_PLATFORM_REQUIRED,
    IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, IOCTL_DISK_GET_LENGTH_INFO,
    IOCTL_DISK_IS_WRITABLE, IOCTL_DISK_UPDATE_PROPERTIES, IOCTL_STORAGE_GET_DEVICE_NUMBER,
    IOCTL_STORAGE_QUERY_PROPERTY, PARTITION_INFORMATION_EX, PARTITION_INFORMATION_GPT,
    PARTITION_INFORMATION_MBR, PARTITION_STYLE_GPT, PARTITION_STYLE_MBR, PARTITION_STYLE_RAW,
    PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR, STORAGE_DEVICE_NUMBER,
    STORAGE_PROPERTY_QUERY, StorageDeviceProperty, VOLUME_DISK_EXTENTS,
};
use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;

use crate::devices::DeviceInfo;

const MAX_BUFFER: usize = 1024 * 1024;

pub(super) struct Guard {
    file: File,
    // Closing these handles releases the volume locks, including on any error path.
    volume_guards: Vec<File>,
    logical_sector: u32,
    capacity: u64,
}

impl Guard {
    pub(super) fn open(expected: &DeviceInfo) -> Result<Self, String> {
        let disk_number = physical_drive_number(&expected.path)?;
        if expected.read_only || expected.size_bytes == 0 {
            return Err("Refusing a read-only or empty USB disk".into());
        }
        // No sharing fallback: another physical-disk writer must not coexist with this guard.
        let file = open_handle(&expected.path, GENERIC_READ | GENERIC_WRITE, 0)?;
        let (logical_sector, capacity) = validate_disk(&file, expected, disk_number)?;
        reject_critical_partitions(&query(&file, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, None)?)?;
        reject_windows_disk(disk_number)?;

        let mut volume_guards = Vec::new();
        let mut locked = BTreeSet::new();
        for name in volume_names()? {
            let volume = open_volume(&name, 0)?;
            let disks = volume_disks(&volume)?;
            if !disks.contains(&disk_number) {
                continue;
            }
            // Never dismount a spanned volume that also uses a different physical disk.
            if disks.iter().any(|&disk| disk != disk_number) {
                return Err(format!("Refusing multi-disk volume {name}"));
            }
            drop(volume);
            let volume = open_volume(&name, GENERIC_READ | GENERIC_WRITE)?;
            if volume_disks(&volume)? != disks {
                return Err("Volume disk extents changed while acquiring exclusive access".into());
            }
            control(&volume, FSCTL_LOCK_VOLUME).map_err(|error| {
                format!("Cannot lock {name}; close applications using it: {error}")
            })?;
            control(&volume, FSCTL_DISMOUNT_VOLUME)?;
            locked.insert(name);
            volume_guards.push(volume);
        }

        // Catch volumes appearing during acquisition. Locked handles stay alive throughout.
        for name in volume_names()? {
            if !locked.contains(&name)
                && volume_disks(&open_volume(&name, 0)?)?.contains(&disk_number)
            {
                return Err(
                    "A new target volume appeared while acquiring disk locks; retry".into(),
                );
            }
        }
        reject_windows_disk(disk_number)?;
        let checked = validate_disk(&file, expected, disk_number)?;
        if checked != (logical_sector, capacity) {
            return Err("Disk geometry changed while acquiring volume locks".into());
        }
        reject_critical_partitions(&query(&file, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, None)?)?;
        Ok(Self {
            file,
            volume_guards,
            logical_sector,
            capacity,
        })
    }

    pub(super) fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    pub(super) fn sector_size(&self) -> u32 {
        self.logical_sector
    }

    pub(super) fn capacity(&self) -> u64 {
        self.capacity
    }

    pub(super) fn finish(&mut self) -> Result<(), String> {
        self.file
            .sync_all()
            .map_err(|error| format!("Flushing raw disk failed: {error}"))?;
        // Keep locks until after the flush and partition-cache refresh, not merely until writing ends.
        let _held_locks = &self.volume_guards;
        control(&self.file, IOCTL_DISK_UPDATE_PROPERTIES)
            .map_err(|error| format!("Refreshing disk properties failed: {error}"))
    }
}

fn physical_drive_number(path: &str) -> Result<u32, String> {
    let suffix = path
        .strip_prefix(r"\\.\PhysicalDrive")
        .ok_or("Expected a canonical Windows physical-drive path")?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("Invalid physical-drive number".into());
    }
    let number: u32 = suffix
        .parse()
        .map_err(|_| "Physical-drive number is too large")?;
    if suffix != number.to_string() {
        return Err("Noncanonical physical-drive number".into());
    }
    Ok(number)
}

fn win_error(operation: &str, code: u32) -> String {
    format!(
        "{operation}: {} (Windows error {code})",
        std::io::Error::from_raw_os_error(code as i32)
    )
}

fn wide(value: &str) -> Result<Vec<u16>, String> {
    if value.contains('\0') {
        return Err("Windows device path contains a NUL".into());
    }
    Ok(value.encode_utf16().chain(Some(0)).collect())
}

fn open_handle(path: &str, access: u32, sharing: u32) -> Result<File, String> {
    let path_w = wide(path)?;
    // SAFETY: path is terminated, other pointers are null, and OPEN_EXISTING cannot create/truncate.
    let handle = unsafe {
        CreateFileW(
            path_w.as_ptr(),
            access,
            sharing,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        // SAFETY: immediately follows the failed call.
        return Err(win_error(&format!("Opening {path}"), unsafe {
            GetLastError()
        }));
    }
    // SAFETY: CreateFileW returned a fresh owned handle, transferred exactly once to File.
    Ok(unsafe { File::from_raw_handle(handle) })
}

fn open_volume(name: &str, access: u32) -> Result<File, String> {
    if !name.starts_with(r"\\?\Volume{") || !name.ends_with("}\\") {
        return Err("Invalid volume GUID path returned by Windows".into());
    }
    open_handle(
        name.trim_end_matches('\\'),
        access,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
    )
}

fn ioctl(
    file: &File,
    code: u32,
    input: Option<&STORAGE_PROPERTY_QUERY>,
    output: &mut [u64],
) -> Result<usize, u32> {
    let mut returned = 0;
    let (input_ptr, input_size) = input.map_or((null(), 0), |value| {
        (
            (value as *const STORAGE_PROPERTY_QUERY).cast(),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
        )
    });
    let bytes = size_of_val(output);
    // SAFETY: initialized query and aligned writable words cover the supplied sizes. The owned
    // synchronous handle remains alive, and no asynchronous operation can outlive these buffers.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            code,
            input_ptr,
            input_size,
            if bytes == 0 {
                null_mut()
            } else {
                output.as_mut_ptr().cast()
            },
            bytes as u32,
            &mut returned,
            null_mut(),
        )
    };
    if ok == 0 {
        // SAFETY: immediately follows the failed call.
        Err(unsafe { GetLastError() })
    } else if returned as usize > bytes {
        Err(ERROR_INSUFFICIENT_BUFFER)
    } else {
        Ok(returned as usize)
    }
}

fn control(file: &File, code: u32) -> Result<(), String> {
    ioctl(file, code, None, &mut [])
        .map(|_| ())
        .map_err(|code| win_error("Disk/volume control", code))
}

fn query(
    file: &File,
    code: u32,
    input: Option<&STORAGE_PROPERTY_QUERY>,
) -> Result<Vec<u8>, String> {
    let mut size = 1024;
    loop {
        let mut words = vec![0_u64; size / 8];
        match ioctl(file, code, input, &mut words) {
            Ok(length) => {
                // SAFETY: every word is initialized, length was bounded by ioctl, and u8 is unaligned.
                return Ok(
                    unsafe { std::slice::from_raw_parts(words.as_ptr().cast(), length) }.to_vec(),
                );
            }
            Err(ERROR_INSUFFICIENT_BUFFER | ERROR_MORE_DATA) if size < MAX_BUFFER => size *= 2,
            Err(error) => return Err(win_error("Querying disk/volume information", error)),
        }
    }
}

fn bytes_at<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], String> {
    bytes
        .get(
            offset
                ..offset
                    .checked_add(N)
                    .ok_or("Storage response offset overflow")?,
        )
        .ok_or_else(|| "Truncated Windows storage response".to_string())?
        .try_into()
        .map_err(|_| "Invalid Windows storage field".into())
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(bytes_at(bytes, offset)?))
}

fn positive_i64(bytes: &[u8], offset: usize) -> Result<u64, String> {
    let value = i64::from_le_bytes(bytes_at(bytes, offset)?);
    if value <= 0 {
        return Err("Disk reports a nonpositive length".into());
    }
    Ok(value as u64)
}

fn descriptor_serial(bytes: &[u8]) -> Result<Option<String>, String> {
    let size = u32_at(bytes, offset_of!(STORAGE_DEVICE_DESCRIPTOR, Size))? as usize;
    let header = offset_of!(STORAGE_DEVICE_DESCRIPTOR, RawDeviceProperties);
    if size < header || size > bytes.len() {
        return Err("Invalid storage descriptor size".into());
    }
    let bytes = &bytes[..size];
    if u32_at(bytes, offset_of!(STORAGE_DEVICE_DESCRIPTOR, BusType))? != BusTypeUsb as u32
        || bytes[offset_of!(STORAGE_DEVICE_DESCRIPTOR, DeviceType)] != 0
    {
        return Err("Opened disk is not USB direct-access storage".into());
    }
    let offset = u32_at(
        bytes,
        offset_of!(STORAGE_DEVICE_DESCRIPTOR, SerialNumberOffset),
    )? as usize;
    if offset == 0 {
        return Ok(None);
    }
    if offset < header {
        return Err("Serial number points into descriptor header".into());
    }
    let tail = bytes
        .get(offset..)
        .ok_or("Serial number is outside descriptor")?;
    let end = tail
        .iter()
        .position(|&byte| byte == 0)
        .ok_or("Unterminated disk serial number")?;
    // Discovery uses the same lossy UTF-8 decoding and whitespace normalization.
    let serial = String::from_utf8_lossy(&tail[..end]).trim().to_owned();
    Ok((!serial.is_empty()).then_some(serial))
}

fn validate_disk(file: &File, expected: &DeviceInfo, disk: u32) -> Result<(u32, u64), String> {
    let number = query(file, IOCTL_STORAGE_GET_DEVICE_NUMBER, None)?;
    if u32_at(&number, offset_of!(STORAGE_DEVICE_NUMBER, DeviceType))? != 7
        || u32_at(&number, offset_of!(STORAGE_DEVICE_NUMBER, DeviceNumber))? != disk
        || !matches!(
            u32_at(&number, offset_of!(STORAGE_DEVICE_NUMBER, PartitionNumber))?,
            0 | u32::MAX
        )
    {
        return Err("Opened handle does not identify the selected whole physical disk".into());
    }
    let property = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let descriptor = query(file, IOCTL_STORAGE_QUERY_PROPERTY, Some(&property))?;
    if descriptor_serial(&descriptor)? != expected.serial {
        return Err("USB disk serial identity changed since selection".into());
    }
    let removable = *descriptor
        .get(offset_of!(STORAGE_DEVICE_DESCRIPTOR, RemovableMedia))
        .ok_or("Truncated removable-media flag")?;
    if removable > 1 || (removable != 0) != expected.removable {
        return Err("USB disk removable-media identity changed since selection".into());
    }
    let geometry = query(file, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, None)?;
    let sector = u32_at(
        &geometry,
        offset_of!(DISK_GEOMETRY_EX, Geometry) + offset_of!(DISK_GEOMETRY, BytesPerSector),
    )?;
    let capacity = positive_i64(&geometry, offset_of!(DISK_GEOMETRY_EX, DiskSize))?;
    let length = positive_i64(&query(file, IOCTL_DISK_GET_LENGTH_INFO, None)?, 0)?;
    if !matches!(sector, 512 | 4096) || capacity % u64::from(sector) != 0 {
        return Err("Unsupported or inconsistent logical disk sector geometry".into());
    }
    if capacity != length || capacity != expected.size_bytes {
        return Err("USB disk capacity changed or storage capacity queries disagree".into());
    }
    control(file, IOCTL_DISK_IS_WRITABLE)?;
    Ok((sector, capacity))
}

fn parse_extents(bytes: &[u8]) -> Result<Vec<u32>, String> {
    let count = u32_at(bytes, offset_of!(VOLUME_DISK_EXTENTS, NumberOfDiskExtents))? as usize;
    let start = offset_of!(VOLUME_DISK_EXTENTS, Extents);
    if count == 0 || count > bytes.len().saturating_sub(start) / size_of::<DISK_EXTENT>() {
        return Err("Invalid or truncated volume disk extents".into());
    }
    let mut disks = Vec::with_capacity(count);
    for index in 0..count {
        let base = start + index * size_of::<DISK_EXTENT>();
        let offset = i64::from_le_bytes(bytes_at(
            bytes,
            base + offset_of!(DISK_EXTENT, StartingOffset),
        )?);
        let length = positive_i64(bytes, base + offset_of!(DISK_EXTENT, ExtentLength))?;
        if offset < 0 || (offset as u64).checked_add(length).is_none() {
            return Err("Invalid volume extent range".into());
        }
        disks.push(u32_at(bytes, base + offset_of!(DISK_EXTENT, DiskNumber))?);
    }
    Ok(disks)
}

fn volume_disks(file: &File) -> Result<Vec<u32>, String> {
    parse_extents(&query(file, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, None)?)
}

struct VolumeSearch(HANDLE);
impl Drop for VolumeSearch {
    fn drop(&mut self) {
        // SAFETY: this owns a successful FindFirstVolumeW search, not a file handle.
        unsafe { FindVolumeClose(self.0) };
    }
}

fn terminated_string(buffer: &[u16]) -> Result<String, String> {
    let end = buffer
        .iter()
        .position(|&value| value == 0)
        .ok_or("Unterminated Windows path")?;
    String::from_utf16(&buffer[..end]).map_err(|_| "Invalid UTF-16 Windows path".into())
}

fn volume_names() -> Result<Vec<String>, String> {
    let mut buffer = vec![0_u16; 1024];
    // SAFETY: writable UTF-16 buffer has the specified capacity.
    let search = unsafe { FindFirstVolumeW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if search == INVALID_HANDLE_VALUE {
        // SAFETY: immediately follows the failed call.
        let error = unsafe { GetLastError() };
        if error == ERROR_NO_MORE_FILES {
            return Ok(Vec::new());
        }
        return Err(win_error("Enumerating volumes", error));
    }
    let search = VolumeSearch(search);
    let mut names = Vec::new();
    loop {
        let name = terminated_string(&buffer)?;
        if names.contains(&name) {
            return Err("Windows returned a duplicate volume".into());
        }
        names.push(name);
        buffer.fill(0);
        // SAFETY: live search handle and writable UTF-16 buffer.
        if unsafe { FindNextVolumeW(search.0, buffer.as_mut_ptr(), buffer.len() as u32) } == 0 {
            // SAFETY: immediately follows the failed call.
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_FILES {
                break;
            }
            return Err(win_error("Enumerating volumes", error));
        }
    }
    Ok(names)
}

fn reject_windows_disk(disk: u32) -> Result<(), String> {
    let mut directory = vec![0_u16; 32768];
    // SAFETY: writable UTF-16 buffer covers the specified capacity.
    let length = unsafe { GetWindowsDirectoryW(directory.as_mut_ptr(), directory.len() as u32) };
    if length == 0 || length as usize >= directory.len() {
        return Err("Cannot identify the running Windows directory".into());
    }
    let mut mount = vec![0_u16; 32768];
    let mut volume = vec![0_u16; 1024];
    // SAFETY: directory is terminated and output buffers cover their specified capacities.
    if unsafe { GetVolumePathNameW(directory.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) }
        == 0
    {
        return Err(win_error("Locating Windows volume", unsafe {
            GetLastError()
        }));
    }
    // SAFETY: mount is terminated by the successful call above; volume is writable.
    if unsafe {
        GetVolumeNameForVolumeMountPointW(mount.as_ptr(), volume.as_mut_ptr(), volume.len() as u32)
    } == 0
    {
        return Err(win_error("Identifying Windows volume", unsafe {
            GetLastError()
        }));
    }
    if volume_disks(&open_volume(&terminated_string(&volume)?, 0)?)?.contains(&disk) {
        return Err(
            "Refusing to overwrite the disk containing the running Windows installation".into(),
        );
    }
    Ok(())
}

fn reject_critical_partitions(bytes: &[u8]) -> Result<(), String> {
    let style = u32_at(
        bytes,
        offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionStyle),
    )?;
    let count = u32_at(
        bytes,
        offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionCount),
    )? as usize;
    let start = offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry);
    if bytes.len() < start
        || count > bytes.len().saturating_sub(start) / size_of::<PARTITION_INFORMATION_EX>()
    {
        return Err("Truncated disk partition layout".into());
    }
    if ![
        PARTITION_STYLE_RAW as u32,
        PARTITION_STYLE_MBR as u32,
        PARTITION_STYLE_GPT as u32,
    ]
    .contains(&style)
        || (style == PARTITION_STYLE_RAW as u32 && count != 0)
    {
        return Err("Unrecognized disk partition layout".into());
    }
    for index in 0..count {
        let base = start + index * size_of::<PARTITION_INFORMATION_EX>();
        if u32_at(
            bytes,
            base + offset_of!(PARTITION_INFORMATION_EX, PartitionStyle),
        )? != style
        {
            return Err("Inconsistent partition styles in disk layout".into());
        }
        let union = base + offset_of!(PARTITION_INFORMATION_EX, Anonymous);
        if bytes[base + offset_of!(PARTITION_INFORMATION_EX, IsServicePartition)] != 0 {
            return Err("Refusing a disk containing a Windows service partition".into());
        }
        if style == PARTITION_STYLE_GPT as u32 {
            let kind = bytes_at::<16>(
                bytes,
                union + offset_of!(PARTITION_INFORMATION_GPT, PartitionType),
            )?;
            let attributes = u64::from_le_bytes(bytes_at(
                bytes,
                union + offset_of!(PARTITION_INFORMATION_GPT, Attributes),
            )?);
            // GUID bytes use Windows little-endian fields: EFI system and Microsoft recovery.
            let efi = [
                0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0, 0xa0, 0xc9, 0x3e,
                0xc9, 0x3b,
            ];
            let recovery = [
                0xa4, 0xbb, 0x94, 0xde, 0xd1, 0x06, 0x40, 0x4d, 0xa1, 0x6a, 0xbf, 0xd5, 0x01, 0x79,
                0xd6, 0xac,
            ];
            if kind == efi || kind == recovery || attributes & GPT_ATTRIBUTE_PLATFORM_REQUIRED != 0
            {
                return Err(
                    "Refusing a disk containing an EFI, recovery, or platform-required partition"
                        .into(),
                );
            }
        } else if style == PARTITION_STYLE_MBR as u32 {
            let kind = bytes[union + offset_of!(PARTITION_INFORMATION_MBR, PartitionType)];
            if matches!(kind, 0x27 | 0xef | 0x42) {
                return Err(
                    "Refusing a disk containing a recovery, EFI, or dynamic-disk partition".into(),
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn extents_include_every_disk_and_reject_truncation() {
        let start = offset_of!(VOLUME_DISK_EXTENTS, Extents);
        let stride = size_of::<DISK_EXTENT>();
        let mut bytes = vec![0; start + stride * 2];
        put(&mut bytes, 0, 2);
        for (index, disk) in [4, 9].into_iter().enumerate() {
            let base = start + index * stride;
            put(&mut bytes, base + offset_of!(DISK_EXTENT, DiskNumber), disk);
            bytes[base + offset_of!(DISK_EXTENT, ExtentLength)..][..8]
                .copy_from_slice(&4096_i64.to_le_bytes());
        }
        assert_eq!(parse_extents(&bytes).unwrap(), [4, 9]);
        assert!(parse_extents(&bytes[..bytes.len() - 1]).is_err());
        put(&mut bytes, 0, u32::MAX);
        assert!(parse_extents(&bytes).is_err());
        put(&mut bytes, 0, 0);
        assert!(parse_extents(&bytes).is_err());
    }

    #[test]
    fn usb_serial_descriptor_is_bounded() {
        let header = offset_of!(STORAGE_DEVICE_DESCRIPTOR, RawDeviceProperties);
        let mut bytes = vec![0; header + 8];
        let size = bytes.len() as u32;
        put(
            &mut bytes,
            offset_of!(STORAGE_DEVICE_DESCRIPTOR, Size),
            size,
        );
        put(
            &mut bytes,
            offset_of!(STORAGE_DEVICE_DESCRIPTOR, BusType),
            BusTypeUsb as u32,
        );
        put(
            &mut bytes,
            offset_of!(STORAGE_DEVICE_DESCRIPTOR, SerialNumberOffset),
            header as u32,
        );
        bytes[header..].copy_from_slice(b" serial\0");
        assert_eq!(
            descriptor_serial(&bytes).unwrap().as_deref(),
            Some("serial")
        );
        bytes[header + 7] = b'x';
        assert!(descriptor_serial(&bytes).is_err());
        put(
            &mut bytes,
            offset_of!(STORAGE_DEVICE_DESCRIPTOR, SerialNumberOffset),
            4,
        );
        assert!(descriptor_serial(&bytes).is_err());
        put(
            &mut bytes,
            offset_of!(STORAGE_DEVICE_DESCRIPTOR, SerialNumberOffset),
            0,
        );
        assert_eq!(descriptor_serial(&bytes).unwrap(), None);
        put(
            &mut bytes,
            offset_of!(STORAGE_DEVICE_DESCRIPTOR, BusType),
            0,
        );
        assert!(descriptor_serial(&bytes).is_err());
    }

    #[test]
    fn physical_paths_cannot_address_partitions_or_arbitrary_files() {
        assert_eq!(physical_drive_number(r"\\.\PhysicalDrive12").unwrap(), 12);
        for path in [
            r"\\.\PhysicalDrive01",
            r"\\.\PhysicalDrive1\Partition1",
            r"C:\file",
            r"\\.\PhysicalDrive",
        ] {
            assert!(physical_drive_number(path).is_err());
        }
    }

    #[test]
    fn truncated_layout_fails_closed() {
        assert!(reject_critical_partitions(&[]).is_err());
        let mut bytes = vec![0; offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry)];
        put(&mut bytes, 0, PARTITION_STYLE_RAW as u32);
        assert!(reject_critical_partitions(&bytes).is_ok());
        put(
            &mut bytes,
            offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionCount),
            1,
        );
        assert!(reject_critical_partitions(&bytes).is_err());
    }

    #[test]
    fn gpt_required_efi_and_recovery_partitions_are_refused() {
        let base = offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry);
        let mut bytes = vec![0; base + size_of::<PARTITION_INFORMATION_EX>()];
        put(&mut bytes, 0, PARTITION_STYLE_GPT as u32);
        put(
            &mut bytes,
            offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionCount),
            1,
        );
        put(&mut bytes, base, PARTITION_STYLE_GPT as u32);
        assert!(reject_critical_partitions(&bytes).is_ok());
        let union = base + offset_of!(PARTITION_INFORMATION_EX, Anonymous);
        let attributes = union + offset_of!(PARTITION_INFORMATION_GPT, Attributes);
        bytes[attributes..attributes + 8]
            .copy_from_slice(&GPT_ATTRIBUTE_PLATFORM_REQUIRED.to_le_bytes());
        assert!(reject_critical_partitions(&bytes).is_err());
        bytes[attributes..attributes + 8].fill(0);
        for kind in [
            [
                0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0, 0xa0, 0xc9, 0x3e,
                0xc9, 0x3b,
            ],
            [
                0xa4, 0xbb, 0x94, 0xde, 0xd1, 0x06, 0x40, 0x4d, 0xa1, 0x6a, 0xbf, 0xd5, 0x01, 0x79,
                0xd6, 0xac,
            ],
        ] {
            bytes[union..union + 16].copy_from_slice(&kind);
            assert!(reject_critical_partitions(&bytes).is_err());
        }
    }
}
