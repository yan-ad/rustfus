use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub path: String,
    pub name: String,
    pub serial: Option<String>,
    pub size_bytes: u64,
    pub removable: bool,
    pub read_only: bool,
}

impl DeviceInfo {
    pub fn display_label(&self) -> String {
        format!(
            "{} ({}) [{}]{}",
            self.name,
            self.path,
            format_bytes(self.size_bytes),
            if self.read_only { " · read-only" } else { "" }
        )
    }
}

pub fn enumerate_usb_devices() -> Result<Vec<DeviceInfo>, String> {
    let mut devices = crate::platform::enumerate()?;
    devices.retain(|device| device.size_bytes > 0);
    devices.sort_by(|left, right| left.path.cmp(&right.path));
    devices.dedup_by(|left, right| left.path == right.path);
    Ok(devices)
}

pub struct DeviceMonitor {
    refresh: SyncSender<()>,
    updates: Arc<Mutex<Option<DeviceUpdate>>>,
}

type DeviceUpdate = Result<Vec<DeviceInfo>, String>;

impl DeviceMonitor {
    pub fn start() -> Self {
        let (refresh, commands) = mpsc::sync_channel(1);
        let updates = Arc::new(Mutex::new(None));
        let mailbox = Arc::clone(&updates);
        thread::Builder::new()
            .name("usb-discovery".into())
            .spawn(move || {
                loop {
                    let result = enumerate_usb_devices();
                    // Overwrite stale snapshots while the window is hidden or suspended.
                    *mailbox.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(result);
                    match commands.recv_timeout(Duration::from_secs(2)) {
                        Ok(()) => {}
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .expect("could not start USB discovery thread");
        Self { refresh, updates }
    }

    pub fn refresh(&self) {
        let _ = self.refresh.try_send(());
    }

    pub fn try_recv(&self) -> Option<Result<Vec<DeviceInfo>, String>> {
        self.updates
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
    }
}

impl Default for DeviceMonitor {
    fn default() -> Self {
        Self::start()
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_decimal_disk_capacity_including_large_disks() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(8_000_000_000), "8.0 GB");
        assert_eq!(format_bytes(2_000_000_000_000), "2.0 TB");
        assert_eq!(format_bytes(u64::MAX), "18.4 EB");
    }

    #[test]
    fn mailbox_retains_latest_result_not_oldest_snapshot() {
        let (refresh, _commands) = mpsc::sync_channel(1);
        let updates = Arc::new(Mutex::new(Some(Err("old scan".into()))));
        let monitor = DeviceMonitor {
            refresh,
            updates: Arc::clone(&updates),
        };
        *updates.lock().unwrap() = Some(Ok(Vec::new()));
        assert_eq!(monitor.try_recv(), Some(Ok(Vec::new())));
        assert!(monitor.try_recv().is_none());
    }
}
