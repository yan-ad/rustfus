use std::fs::File;

use crate::devices::{DeviceInfo, enumerate_usb_devices};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

pub(crate) struct NativeDisk {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    guard: platform::Guard,
    #[cfg(target_os = "linux")]
    file: File,
    #[cfg(target_os = "linux")]
    sector_size: u32,
    #[cfg(target_os = "linux")]
    capacity: u64,
}

impl NativeDisk {
    pub(crate) fn open(expected: &DeviceInfo) -> Result<Self, String> {
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            let _ = expected;
            Err("Native disk access is supported on Linux, macOS, and Windows only".into())
        }
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            revalidate(expected)?;
            #[cfg(target_os = "linux")]
            let disk = {
                let (file, sector_size, capacity) = linux::open(expected)?;
                Self {
                    file,
                    sector_size,
                    capacity,
                }
            };
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            let disk = Self {
                guard: platform::Guard::open(expected)?,
            };
            if !matches!(disk.sector_size(), 512 | 4096)
                || disk.capacity() != expected.size_bytes
                || !disk
                    .capacity()
                    .is_multiple_of(u64::from(disk.sector_size()))
            {
                return Err("Unsupported or changed native disk geometry".into());
            }
            revalidate(expected)?;
            Ok(disk)
        }
    }

    pub(crate) fn file_mut(&mut self) -> &mut File {
        #[cfg(target_os = "linux")]
        {
            &mut self.file
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.guard.file_mut()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            unreachable!("native disk access is unsupported")
        }
    }

    pub(crate) fn sector_size(&self) -> u32 {
        #[cfg(target_os = "linux")]
        {
            self.sector_size
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.guard.sector_size()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            unreachable!("native disk access is unsupported")
        }
    }

    pub(crate) fn capacity(&self) -> u64 {
        #[cfg(target_os = "linux")]
        {
            self.capacity
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.guard.capacity()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            unreachable!("native disk access is unsupported")
        }
    }

    pub(crate) fn synchronize(&mut self) -> Result<(), String> {
        self.file_mut()
            .sync_all()
            .map_err(|e| format!("Synchronizing raw disk: {e}"))
    }

    pub(crate) fn finish(&mut self) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        {
            linux::finish(&self.file)
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.guard.finish()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            Err("Native disk access is unsupported on this platform".into())
        }
    }
}

fn revalidate(expected: &DeviceInfo) -> Result<(), String> {
    let devices = enumerate_usb_devices()?;
    let current = devices
        .iter()
        .find(|disk| disk.path == expected.path)
        .ok_or("The selected USB disk is no longer present; rediscover it")?;
    validate_identity(expected, current)
}

fn validate_identity(expected: &DeviceInfo, current: &DeviceInfo) -> Result<(), String> {
    if expected.read_only
        || expected.size_bytes == 0
        || current.read_only
        || expected.id != current.id
        || expected.path != current.path
        || expected.serial != current.serial
        || expected.size_bytes != current.size_bytes
        || expected.read_only != current.read_only
        || expected.removable != current.removable
    {
        return Err("The selected USB disk's identity, capacity, or writable state changed; select it again".into());
    }
    // Fixed USB disks remain eligible when explicitly selected by the caller.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> DeviceInfo {
        DeviceInfo {
            id: "usb-id".into(),
            path: "/dev/test-only".into(),
            name: "USB disk".into(),
            serial: Some("serial".into()),
            size_bytes: 4096,
            removable: true,
            read_only: false,
        }
    }

    #[test]
    fn identity_changes_fail_closed_but_labels_are_not_identity() {
        let expected = device();
        let mut renamed = expected.clone();
        renamed.name = "New display label".into();
        assert!(validate_identity(&expected, &renamed).is_ok());
        for field in 0..7 {
            let mut changed = expected.clone();
            match field {
                0 => changed.id.push('x'),
                1 => changed.path.push('x'),
                2 => changed.serial = None,
                3 => changed.size_bytes += 512,
                4 => changed.read_only = true,
                5 => changed.removable = false,
                _ => changed.size_bytes = 0,
            }
            assert!(validate_identity(&expected, &changed).is_err());
        }
        let mut fixed = expected.clone();
        fixed.removable = false;
        assert!(validate_identity(&fixed, &fixed).is_ok());
        fixed.read_only = true;
        assert!(validate_identity(&fixed, &fixed).is_err());
    }

    #[test]
    fn native_disk_can_be_owned_by_the_writer_thread() {
        fn assert_send<T: Send>() {}
        assert_send::<NativeDisk>();
    }
}
