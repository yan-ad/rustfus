use std::fs::File;
use std::path::{Path, PathBuf};

use crate::devices::DeviceInfo;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BootSelection {
    #[default]
    NonBootable,
    MsDos,
    FreeDos,
    Iso,
}

impl BootSelection {
    pub fn label(self) -> &'static str {
        match self {
            Self::NonBootable => "Non Bootable",
            Self::MsDos => "MS-DOS",
            Self::FreeDos => "FreeDOS",
            Self::Iso => "Disk or ISO image",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PartitionScheme {
    #[default]
    Mbr,
    Gpt,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TargetSystem {
    #[default]
    BiosOrUefi,
    Legacy,
}

#[derive(Debug, PartialEq, Eq)]
pub struct IsoImage {
    pub path: PathBuf,
    pub name: String,
    pub size_bytes: u64,
}

impl IsoImage {
    pub fn open(path: PathBuf) -> Result<Self, String> {
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("iso"))
        {
            return Err("Choose an image with an .iso extension.".into());
        }
        // Validate through the opened handle, rather than racing separate path checks.
        let file = File::open(&path).map_err(|error| format!("Cannot open image: {error}"))?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("Cannot inspect image: {error}"))?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err("Choose a non-empty, regular ISO file.".into());
        }
        let name = path
            .file_name()
            .ok_or("Image has no filename.")?
            .to_string_lossy()
            .into_owned();
        Ok(Self {
            path,
            name,
            size_bytes: metadata.len(),
        })
    }
}

#[derive(Debug, Default)]
pub struct AppModel {
    all_devices: Vec<DeviceInfo>,
    require_explicit_selection: bool,
    pub devices: Vec<DeviceInfo>,
    pub selected_device: Option<String>,
    pub include_usb_hard_drives: bool,
    pub boot_selection: BootSelection,
    pub iso: Option<IsoImage>,
    pub partition_scheme: PartitionScheme,
    pub target_system: TargetSystem,
}

impl AppModel {
    pub fn selected_device(&self) -> Option<&DeviceInfo> {
        self.selected_device
            .as_deref()
            .and_then(|id| self.devices.iter().find(|device| device.id == id))
    }

    pub fn apply_devices(&mut self, devices: Vec<DeviceInfo>) {
        self.all_devices = devices;
        self.refilter();
    }

    pub fn set_include_fixed(&mut self, include: bool) {
        self.include_usb_hard_drives = include;
        self.refilter();
    }

    fn refilter(&mut self) {
        let previous = self.selected_device.clone();
        self.devices = self
            .all_devices
            .iter()
            .filter(|device| {
                device.size_bytes > 0 && (device.removable || self.include_usb_hard_drives)
            })
            .cloned()
            .collect();
        self.devices
            .sort_by(|left, right| left.path.cmp(&right.path));
        let was_selected = previous.is_some();
        self.selected_device =
            previous.filter(|id| self.devices.iter().any(|device| &device.id == id));
        if was_selected && self.selected_device.is_none() {
            self.require_explicit_selection = true;
        }
        // Never silently switch the target when the selected drive disappears.
        if !self.require_explicit_selection
            && self.selected_device.is_none()
            && self.devices.len() == 1
        {
            self.selected_device = self.devices.first().map(|device| device.id.clone());
        }
    }

    pub fn choose_iso(&mut self, path: PathBuf) -> Result<(), String> {
        let image = IsoImage::open(path)?;
        self.iso = Some(image);
        self.boot_selection = BootSelection::Iso;
        Ok(())
    }

    pub fn boot_selection_label(&self) -> &str {
        if self.boot_selection == BootSelection::Iso {
            self.iso
                .as_ref()
                .map_or("Disk or ISO image (select a file)", |iso| iso.name.as_str())
        } else {
            self.boot_selection.label()
        }
    }

    pub fn selected_iso_path(&self) -> Option<&Path> {
        (self.boot_selection == BootSelection::Iso)
            .then_some(self.iso.as_ref())
            .flatten()
            .map(|iso| iso.path.as_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, removable: bool) -> DeviceInfo {
        DeviceInfo {
            id: id.into(),
            path: format!("/dev/{id}"),
            name: "USB disk".into(),
            serial: None,
            size_bytes: 8_000_000_000,
            removable,
            read_only: false,
        }
    }

    #[test]
    fn starts_empty_without_invented_devices_or_images() {
        let model = AppModel::default();
        assert!(model.devices.is_empty());
        assert!(model.selected_device().is_none());
        assert!(model.iso.is_none());
    }

    #[test]
    fn selection_survives_reorder_and_is_cleared_on_unplug() {
        let mut model = AppModel::default();
        model.apply_devices(vec![device("one", true)]);
        assert_eq!(model.selected_device.as_deref(), Some("one"));
        model.apply_devices(vec![device("two", true), device("one", true)]);
        assert_eq!(model.selected_device.as_deref(), Some("one"));
        model.apply_devices(vec![]);
        assert!(model.selected_device.is_none());
        assert!(model.devices.is_empty());
    }

    #[test]
    fn does_not_arbitrarily_select_among_multiple_disks() {
        let mut model = AppModel::default();
        model.apply_devices(vec![device("one", true), device("two", true)]);
        assert!(model.selected_device.is_none());
    }

    #[test]
    fn unplug_never_switches_to_remaining_disk_even_on_later_polls() {
        let mut model = AppModel::default();
        model.apply_devices(vec![device("one", true)]);
        model.apply_devices(vec![device("one", true), device("two", true)]);
        for _ in 0..3 {
            model.apply_devices(vec![device("two", true)]);
            assert!(model.selected_device.is_none());
        }
        model.selected_device = Some("two".into());
        model.apply_devices(vec![device("two", true)]);
        assert_eq!(model.selected_device.as_deref(), Some("two"));
    }

    #[test]
    fn fixed_usb_filter_changes_immediately_without_another_scan() {
        let mut model = AppModel::default();
        model.apply_devices(vec![device("flash", true), device("ssd", false)]);
        assert_eq!(model.devices.len(), 1);
        model.set_include_fixed(true);
        assert_eq!(model.devices.len(), 2);
        model.selected_device = Some("ssd".into());
        model.set_include_fixed(false);
        assert_eq!(model.devices.len(), 1);
        assert_ne!(model.selected_device.as_deref(), Some("ssd"));
    }

    #[test]
    fn iso_selection_uses_real_filename_and_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Install image.ISO");
        std::fs::write(&path, b"readable image fixture; not a bootability check").unwrap();
        let mut model = AppModel::default();
        model.choose_iso(path.clone()).unwrap();
        assert_eq!(model.boot_selection, BootSelection::Iso);
        assert_eq!(model.boot_selection_label(), "Install image.ISO");
        assert_eq!(model.selected_iso_path(), Some(path.as_path()));
        assert_eq!(
            model.iso.as_ref().unwrap().size_bytes,
            std::fs::metadata(path).unwrap().len()
        );
        model.boot_selection = BootSelection::NonBootable;
        assert!(model.selected_iso_path().is_none());
        assert!(model.iso.is_some());
    }

    #[test]
    fn invalid_file_does_not_replace_previous_image() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.iso");
        std::fs::write(&good, b"image").unwrap();
        let mut model = AppModel::default();
        model.choose_iso(good.clone()).unwrap();
        for path in [dir.path().join("missing.iso"), dir.path().join("bad.txt")] {
            assert!(model.choose_iso(path).is_err());
            assert_eq!(model.iso.as_ref().unwrap().path, good);
        }
        let empty = dir.path().join("empty.iso");
        std::fs::write(&empty, b"").unwrap();
        assert!(model.choose_iso(empty).is_err());
        let directory = dir.path().join("folder.iso");
        std::fs::create_dir(&directory).unwrap();
        assert!(model.choose_iso(directory).is_err());
    }
}
