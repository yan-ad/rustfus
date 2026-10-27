#[cfg(any(target_os = "linux", test))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use crate::devices::DeviceInfo;

pub(crate) fn enumerate() -> Result<Vec<DeviceInfo>, String> {
    #[cfg(target_os = "linux")]
    return linux::enumerate();
    #[cfg(target_os = "macos")]
    return macos::enumerate();
    #[cfg(target_os = "windows")]
    return windows::enumerate();
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    Err("Device discovery is supported on Linux, macOS, and Windows only".into())
}
