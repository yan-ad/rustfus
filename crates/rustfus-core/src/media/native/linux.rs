#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::devices::DeviceInfo;

// BLKGETSIZE64 encodes sizeof(size_t), despite returning a u64 (Linux blkpg.h).
const BLKGETSIZE64: u64 = libc::_IOR::<libc::size_t>(0x12, 114) as u64;
const BLKGETDISKSEQ: u64 = libc::_IOR::<u64>(0x12, 128) as u64;
const BLKROGET: u64 = libc::_IO(0x12, 94) as u64;
const BLKRRPART: u64 = libc::_IO(0x12, 95) as u64;

type DeviceNumber = (u32, u32);

#[derive(Debug, PartialEq, Eq)]
struct Registry {
    canonical: PathBuf,
    device: DeviceNumber,
    devices: BTreeSet<DeviceNumber>,
    capacity: u64,
    sector: u32,
    diskseq: Option<u64>,
}

pub(super) fn open(expected: &DeviceInfo) -> Result<(File, u32, u64), String> {
    if expected.read_only || expected.size_bytes == 0 {
        return Err("Refusing a read-only or empty disk".into());
    }
    let path = Path::new(&expected.path);
    let name = path.file_name().ok_or("Missing disk basename")?;
    if path != Path::new("/dev").join(name) {
        return Err("The disk must be a direct /dev block device, not an alias".into());
    }
    let registry_path = Path::new("/sys/class/block").join(name);
    let before = inspect_registry(&registry_path)?;
    if before.capacity != expected.size_bytes {
        return Err("Disk capacity changed since discovery".into());
    }
    let metadata = fs::symlink_metadata(path).map_err(|e| format!("Inspect disk: {e}"))?;
    if !metadata.file_type().is_block_device() || device_number(metadata.rdev()) != before.device {
        return Err("Disk path does not match the registered block device".into());
    }
    refuse_in_use(&before.devices)?;

    // O_EXCL on a block device holds the kernel's exclusive claim until this File is dropped.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_EXCL)
        .open(path)
        .map_err(|e| format!("Cannot exclusively open {}: {e}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("Inspect disk descriptor: {e}"))?;
    if !metadata.file_type().is_block_device() || device_number(metadata.rdev()) != before.device {
        return Err("Opened descriptor does not match the registered disk".into());
    }
    let mut capacity = 0_u64;
    let mut sector = 0_i32;
    let mut read_only = 0_i32;
    ioctl_read(&file, BLKGETSIZE64, &mut capacity).map_err(|e| format!("BLKGETSIZE64: {e}"))?;
    ioctl_read(&file, libc::BLKSSZGET, &mut sector).map_err(|e| format!("BLKSSZGET: {e}"))?;
    ioctl_read(&file, BLKROGET, &mut read_only).map_err(|e| format!("BLKROGET: {e}"))?;
    if read_only != 0 {
        return Err("The opened disk is read-only".into());
    }
    if capacity != expected.size_bytes || sector <= 0 || sector as u32 != before.sector {
        return Err("Opened disk geometry differs from the registry or discovery".into());
    }
    let mut sequence = 0_u64;
    match ioctl_read(&file, BLKGETDISKSEQ, &mut sequence) {
        Ok(()) if before.diskseq == Some(sequence) => {}
        Ok(()) => return Err("Disk sequence changed or cannot be verified against sysfs".into()),
        Err(e) if e.raw_os_error() == Some(libc::ENOTTY) && before.diskseq.is_none() => {}
        Err(e) => return Err(format!("BLKGETDISKSEQ: {e}")),
    }
    let after = inspect_registry(&registry_path)?;
    if before != after {
        return Err("Disk registry changed while opening the device".into());
    }
    refuse_in_use(&after.devices)?;
    Ok((file, sector as u32, capacity))
}

pub(super) fn finish(file: &File) -> Result<(), String> {
    file.sync_all().map_err(|e| format!("Sync disk: {e}"))?;
    // BLKRRPART has no pointer argument; keep the exclusive claim during the reread.
    if unsafe { libc::ioctl(file.as_raw_fd(), BLKRRPART as _, 0) } < 0 {
        return Err(format!(
            "Reread disk partition table: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn ioctl_read<T>(file: &File, request: u64, value: &mut T) -> io::Result<()> {
    // Each caller supplies the exact writable output type required by the block ioctl ABI.
    if unsafe { libc::ioctl(file.as_raw_fd(), request as _, value as *mut T) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn inspect_registry(path: &Path) -> Result<Registry, String> {
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("Resolve disk registry: {e}"))?;
    if !canonical.starts_with("/sys/devices") || exists(&canonical.join("partition"))? {
        return Err("Target is not a whole physical disk in sysfs".into());
    }
    let mut usb = false;
    for ancestor in canonical.ancestors() {
        let vendor = ancestor.join("idVendor");
        let product = ancestor.join("idProduct");
        if exists(&vendor)? || exists(&product)? {
            for attribute in [&vendor, &product] {
                let text = fs::read_to_string(attribute)
                    .map_err(|e| format!("Read USB identity {}: {e}", attribute.display()))?;
                if text.trim().len() != 4 || u16::from_str_radix(text.trim(), 16).is_err() {
                    return Err("Invalid USB vendor/product identity".into());
                }
            }
            usb = true;
            break;
        }
    }
    if !usb || number(&canonical.join("device/type"))? != 0 {
        return Err("Target is not a USB direct-access disk".into());
    }
    if number(&canonical.join("ro"))? != 0 {
        return Err("Disk registry reports read-only media".into());
    }
    let capacity = number(&canonical.join("size"))?
        .checked_mul(512)
        .filter(|size| *size != 0)
        .ok_or("Invalid disk capacity")?;
    let sector = u32::try_from(number(&canonical.join("queue/logical_block_size"))?)
        .map_err(|_| "Invalid logical sector size")?;
    if sector < 512 || !sector.is_power_of_two() || capacity % u64::from(sector) != 0 {
        return Err("Invalid disk geometry".into());
    }
    let device = sysfs_device(&canonical)?;
    let mut devices = BTreeSet::from([device]);
    refuse_holders(&canonical)?;
    for entry in fs::read_dir(&canonical).map_err(|e| format!("Enumerate disk children: {e}"))? {
        let entry = entry.map_err(|e| format!("Read disk child: {e}"))?;
        let kind = entry
            .file_type()
            .map_err(|e| format!("Inspect disk child: {e}"))?;
        if !kind.is_dir() && !kind.is_symlink() {
            continue;
        }
        let child = entry.path();
        if exists(&child.join("partition"))? {
            let resolved = child
                .canonicalize()
                .map_err(|e| format!("Resolve partition: {e}"))?;
            if resolved.parent() != Some(canonical.as_path()) {
                return Err("Partition registry escaped the disk ancestry".into());
            }
            refuse_holders(&resolved)?;
            if !devices.insert(sysfs_device(&resolved)?) {
                return Err("Duplicate device number in disk registry".into());
            }
        }
    }
    let diskseq = if exists(&canonical.join("diskseq"))? {
        Some(number(&canonical.join("diskseq"))?)
    } else {
        None
    };
    Ok(Registry {
        canonical,
        device,
        devices,
        capacity,
        sector,
        diskseq,
    })
}

fn exists(path: &Path) -> Result<bool, String> {
    path.try_exists()
        .map_err(|e| format!("Inspect {}: {e}", path.display()))
}

fn number(path: &Path) -> Result<u64, String> {
    fs::read_to_string(path)
        .map_err(|e| format!("Read {}: {e}", path.display()))?
        .trim()
        .parse()
        .map_err(|e| format!("Invalid {}: {e}", path.display()))
}

fn sysfs_device(path: &Path) -> Result<DeviceNumber, String> {
    let text = fs::read_to_string(path.join("dev"))
        .map_err(|e| format!("Read block device number: {e}"))?;
    parse_device_number(text.trim().as_bytes())
}

fn refuse_holders(path: &Path) -> Result<(), String> {
    let mut holders = fs::read_dir(path.join("holders"))
        .map_err(|e| format!("Inspect block device holders: {e}"))?;
    match holders.next() {
        None => Ok(()),
        Some(Ok(_)) => Err("Disk or partition is held by another block device (dm/md)".into()),
        Some(Err(e)) => Err(format!("Read block device holder: {e}")),
    }
}

fn device_number(device: u64) -> DeviceNumber {
    (libc::major(device), libc::minor(device))
}

fn parse_device_number(text: &[u8]) -> Result<DeviceNumber, String> {
    let text = std::str::from_utf8(text).map_err(|_| "Non-ASCII device number")?;
    let (major, minor) = text
        .split_once(':')
        .ok_or("Missing device number separator")?;
    Ok((
        major.parse().map_err(|_| "Invalid device major")?,
        minor.parse().map_err(|_| "Invalid device minor")?,
    ))
}

fn refuse_in_use(devices: &BTreeSet<DeviceNumber>) -> Result<(), String> {
    let mounts = fs::read("/proc/self/mountinfo").map_err(|e| format!("Read mountinfo: {e}"))?;
    refuse_mounts(&mounts, devices)?;
    let swaps = fs::read("/proc/swaps").map_err(|e| format!("Read swaps: {e}"))?;
    for path in swap_paths(&swaps)? {
        let metadata = fs::metadata(&path)
            .map_err(|e| format!("Inspect active swap {}: {e}", path.display()))?;
        let backing = if metadata.file_type().is_block_device() {
            metadata.rdev()
        } else if metadata.is_file() {
            metadata.dev()
        } else {
            return Err("Active swap is neither a block device nor a regular file".into());
        };
        if devices.contains(&device_number(backing)) {
            return Err("Disk contains active swap".into());
        }
    }
    Ok(())
}

fn refuse_mounts(mountinfo: &[u8], devices: &BTreeSet<DeviceNumber>) -> Result<(), String> {
    if mountinfo.is_empty() {
        return Err("Empty mountinfo cannot establish disk safety".into());
    }
    for line in mountinfo
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        let fields: Vec<_> = line.split(|b| *b == b' ').collect();
        let separator = fields
            .iter()
            .position(|field| *field == b"-")
            .ok_or("Malformed mountinfo separator")?;
        if separator < 6 || fields.len() != separator + 4 || fields.iter().any(|f| f.is_empty()) {
            return Err("Malformed mountinfo record".into());
        }
        let device = parse_device_number(fields[2])?;
        let _root = decode_proc_path(fields[3])?;
        let mountpoint = decode_proc_path(fields[4])?;
        let _source = decode_proc_path(fields[separator + 2])?;
        // Major:minor, not source path or rw flags, also catches read-only bind mounts.
        if devices.contains(&device) {
            return Err(format!(
                "Disk contains a mounted filesystem at {}",
                mountpoint.display()
            ));
        }
    }
    Ok(())
}

fn swap_paths(swaps: &[u8]) -> Result<Vec<PathBuf>, String> {
    let mut lines = swaps.split(|b| *b == b'\n').filter(|line| !line.is_empty());
    let header = lines.next().ok_or("Missing /proc/swaps header")?;
    let header_fields: Vec<_> = header
        .split(u8::is_ascii_whitespace)
        .filter(|f| !f.is_empty())
        .collect();
    if header_fields
        != [
            b"Filename".as_slice(),
            b"Type",
            b"Size",
            b"Used",
            b"Priority",
        ]
    {
        return Err("Invalid /proc/swaps header".into());
    }
    lines
        .map(|line| {
            let fields: Vec<_> = line
                .split(u8::is_ascii_whitespace)
                .filter(|f| !f.is_empty())
                .collect();
            if fields.len() != 5 || !matches!(fields[1], b"file" | b"partition") {
                return Err("Malformed active swap record".into());
            }
            let path = decode_proc_path(fields[0])?;
            if !path.is_absolute() {
                return Err("Active swap path is not absolute".into());
            }
            Ok(path)
        })
        .collect()
}

fn decode_proc_path(encoded: &[u8]) -> Result<PathBuf, String> {
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut i = 0;
    while i < encoded.len() {
        if encoded[i] == b'\\' {
            let octal = encoded
                .get(i + 1..i + 4)
                .ok_or("Truncated proc path escape")?;
            let value = match octal {
                b"040" => b' ',
                b"011" => b'\t',
                b"012" => b'\n',
                b"134" => b'\\',
                _ => return Err("Invalid proc path escape".into()),
            };
            decoded.push(value);
            i += 4;
        } else {
            if encoded[i] == 0 {
                return Err("NUL in proc path".into());
            }
            decoded.push(encoded[i]);
            i += 1;
        }
    }
    Ok(PathBuf::from(OsString::from_vec(decoded)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_proc_paths_without_losing_non_utf8_names() {
        assert_eq!(
            decode_proc_path(b"/a\\040b\\011c\\012d\\134e\xff").unwrap(),
            PathBuf::from(OsString::from_vec(b"/a b\tc\nd\\e\xff".to_vec()))
        );
        for path in [
            b"/bad\\".as_slice(),
            b"/bad\\00",
            b"/bad\\000",
            b"/bad\\777",
        ] {
            assert!(decode_proc_path(path).is_err());
        }
    }

    #[test]
    fn refuses_readonly_bind_mounts_by_device_not_source() {
        let devices = BTreeSet::from([(8, 16), (8, 17)]);
        let bind = b"51 24 8:17 /folder\\040name /home/user ro,nosuid shared:2 - ext4 /dev/disk/by-uuid/other ro\n";
        assert!(
            refuse_mounts(bind, &devices)
                .unwrap_err()
                .contains("mounted")
        );
        assert!(refuse_mounts(b"1 0 8:16 / /boot ro - ext4 /dev/unknown ro\n", &devices).is_err());
        assert!(refuse_mounts(b"1 0 8:1 / / rw - ext4 /dev/sdb1 rw\n", &devices).is_ok());
    }

    #[test]
    fn rescanning_detects_a_mount_added_after_the_first_snapshot() {
        let devices = BTreeSet::from([(8, 16), (8, 17)]);
        let initial = b"1 0 8:1 / / rw - ext4 /dev/sda1 rw\n";
        let changed =
            b"1 0 8:1 / / rw - ext4 /dev/sda1 rw\n2 1 8:17 / /media/usb ro - vfat /dev/sdb1 ro\n";
        assert!(refuse_mounts(initial, &devices).is_ok());
        assert!(refuse_mounts(changed, &devices).is_err());
    }

    #[test]
    fn parses_both_swap_devices_and_escaped_swapfiles() {
        let swaps = b"Filename\tType\tSize\tUsed\tPriority\n/dev/sdb2 partition 1024 0 -2\n/home/swap\\040file file 1024 0 -3\n";
        assert_eq!(
            swap_paths(swaps).unwrap(),
            [PathBuf::from("/dev/sdb2"), PathBuf::from("/home/swap file")]
        );
        assert!(
            swap_paths(b"Filename Type Size Used Priority\n")
                .unwrap()
                .is_empty()
        );
        assert!(swap_paths(b"Filename Type Size Used Priority\nrelative file 1 0 0\n").is_err());
    }

    #[test]
    fn malformed_usage_records_fail_closed() {
        let devices = BTreeSet::from([(8, 16)]);
        assert!(refuse_mounts(b"", &devices).is_err());
        assert!(refuse_mounts(b"1 0 broken / / rw - ext4 /dev/sda1 rw\n", &devices).is_err());
        assert!(refuse_mounts(b"1 0 8:1 / / rw ext4 /dev/sda1 rw\n", &devices).is_err());
        assert!(swap_paths(b"").is_err());
        assert!(swap_paths(b"Filename Type Size Used Priority\n/dev/sdb1 partition\n").is_err());
    }
}
