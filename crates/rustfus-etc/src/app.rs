use std::time::Duration;

use iced::{Element, Subscription, Task};
use rustfus_core::devices::DeviceMonitor;
use rustfus_core::model::{AppModel, BootSelection, IsoImage, PartitionScheme, TargetSystem};

use crate::components::Choice;

#[derive(Debug, Clone)]
pub(crate) enum Message {
    DeviceUpdate,
    SystemThemeChanged(iced::theme::Mode),
    SelectDevice(Choice<String>),
    SelectBoot(Choice<BootSelection>),
    SelectPartition(PartitionScheme),
    SelectTarget(Choice<TargetSystem>),
    IncludeFixed(bool),
    ToggleAdvanced,
    ChooseImage,
    ImageChosen(Option<Result<IsoImage, String>>),
    Close,
}

fn native_theme(mode: iced::theme::Mode) -> iced::Theme {
    let mode = match mode {
        iced::theme::Mode::Dark => shadcn_iced::Mode::Dark,
        iced::theme::Mode::Light | iced::theme::Mode::None => shadcn_iced::Mode::Light,
    };
    shadcn_iced::theme::neutral(mode).build()
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) enum Discovery {
    #[default]
    Loading,
    Ready,
    Failed(String),
}

pub(crate) struct App {
    pub state: State,
    monitor: DeviceMonitor,
}

pub(crate) struct State {
    pub model: AppModel,
    pub discovery: Discovery,
    pub image_error: Option<String>,
    pub choosing_image: bool,
    pub advanced_open: bool,
    pub device_options: Vec<Choice<String>>,
    pub target_options: [Choice<TargetSystem>; 2],
    pub theme: iced::Theme,
}

impl Default for State {
    fn default() -> Self {
        Self {
            model: AppModel::default(),
            discovery: Discovery::Loading,
            image_error: None,
            choosing_image: false,
            advanced_open: false,
            device_options: Vec::new(),
            target_options: [
                Choice::new(TargetSystem::BiosOrUefi, "BIOS or UEFI"),
                Choice::new(TargetSystem::Legacy, "Legacy"),
            ],
            theme: shadcn_iced::theme::neutral(shadcn_iced::Mode::Light).build(),
        }
    }
}

impl State {
    pub fn apply_devices(
        &mut self,
        result: Result<Vec<rustfus_core::devices::DeviceInfo>, String>,
    ) {
        match result {
            Ok(devices) => {
                self.model.apply_devices(devices);
                self.discovery = Discovery::Ready;
            }
            Err(error) => {
                self.model.apply_devices(Vec::new());
                self.discovery = Discovery::Failed(error);
            }
        }
        self.update_device_options();
    }

    pub fn update_device_options(&mut self) {
        self.device_options = self
            .model
            .devices
            .iter()
            .map(|device| Choice::new(device.id.clone(), device.display_label()))
            .collect();
    }

    fn apply_system_theme(&mut self, mode: iced::theme::Mode) {
        self.theme = native_theme(mode);
    }

    fn select_boot(&mut self, selection: BootSelection) {
        if matches!(selection, BootSelection::NonBootable | BootSelection::Iso) {
            self.model.boot_selection = selection;
        }
    }

    pub fn apply_image(&mut self, result: Option<Result<IsoImage, String>>) {
        self.choosing_image = false;
        match result {
            Some(Ok(image)) => {
                self.model.iso = Some(image);
                self.model.boot_selection = BootSelection::Iso;
                self.image_error = None;
            }
            Some(Err(error)) => self.image_error = Some(error),
            // Cancelling preserves the last valid image and its mode.
            None => {}
        }
    }
}

impl App {
    pub fn boot() -> (Self, Task<Message>) {
        (
            Self::new(),
            iced::system::theme().map(Message::SystemThemeChanged),
        )
    }

    pub fn new() -> Self {
        Self {
            state: State::default(),
            monitor: DeviceMonitor::start(),
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SystemThemeChanged(mode) => self.state.apply_system_theme(mode),
            Message::DeviceUpdate => {
                if let Some(result) = self.monitor.try_recv() {
                    self.state.apply_devices(result);
                }
            }
            Message::SelectDevice(choice) => {
                if self
                    .state
                    .model
                    .devices
                    .iter()
                    .any(|device| device.id == choice.value)
                {
                    self.state.model.selected_device = Some(choice.value);
                }
            }
            Message::SelectBoot(choice) => {
                self.state.select_boot(choice.value);
            }
            Message::SelectPartition(scheme) => self.state.model.partition_scheme = scheme,
            Message::SelectTarget(choice) => self.state.model.target_system = choice.value,
            Message::IncludeFixed(include) => {
                self.state.model.set_include_fixed(include);
                self.state.update_device_options();
            }
            Message::ToggleAdvanced => self.state.advanced_open = !self.state.advanced_open,
            Message::ChooseImage if !self.state.choosing_image => {
                self.state.choosing_image = true;
                return Task::perform(pick_iso(), Message::ImageChosen);
            }
            Message::ChooseImage => {}
            Message::ImageChosen(result) => self.state.apply_image(result),
            Message::Close => return iced::exit(),
        }
        Task::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_millis(250)).map(|_| Message::DeviceUpdate),
            iced::system::theme_changes().map(Message::SystemThemeChanged),
        ])
    }

    pub fn theme(&self) -> iced::Theme {
        self.state.theme.clone()
    }

    pub fn view(&self) -> Element<'_, Message> {
        crate::view::view(&self.state)
    }
}

async fn pick_iso() -> Option<Result<IsoImage, String>> {
    let file = rfd::AsyncFileDialog::new()
        .set_title("Select ISO image")
        .add_filter("ISO images", &["iso", "ISO"])
        .pick_file()
        .await?;
    let path = file.path().to_path_buf();
    Some(
        tokio::task::spawn_blocking(move || IsoImage::open(path))
            .await
            .unwrap_or_else(|error| Err(format!("Image inspection failed: {error}"))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_failure_clears_stale_targets_and_recovers_automatically() {
        let mut state = State::default();
        state.apply_devices(Err("temporary device error".into()));
        assert!(matches!(state.discovery, Discovery::Failed(_)));
        assert!(state.device_options.is_empty());
        assert!(state.model.selected_device.is_none());
        state.apply_devices(Ok(vec![]));
        assert_eq!(state.discovery, Discovery::Ready);
    }

    #[test]
    fn cancelled_picker_preserves_image_and_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("system.iso");
        std::fs::write(&path, b"image fixture").unwrap();
        let mut state = State::default();
        state.apply_image(Some(IsoImage::open(path.clone())));
        state.choosing_image = true;
        state.apply_image(None);
        assert!(!state.choosing_image);
        assert_eq!(state.model.iso.as_ref().unwrap().path, path);
        assert_eq!(state.model.boot_selection, BootSelection::Iso);
    }

    #[test]
    fn failed_picker_inspection_preserves_valid_image_and_shows_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("system.iso");
        std::fs::write(&path, b"image fixture").unwrap();
        let mut state = State::default();
        state.apply_image(Some(IsoImage::open(path.clone())));
        state.apply_image(Some(Err("cannot read new image".into())));
        assert_eq!(state.model.iso.as_ref().unwrap().path, path);
        assert!(state.image_error.is_some());
    }

    #[test]
    fn unsupported_dos_selections_cannot_replace_supported_modes() {
        let mut state = State::default();
        for supported in [BootSelection::NonBootable, BootSelection::Iso] {
            state.select_boot(supported);
            assert_eq!(state.model.boot_selection, supported);
            for unsupported in [BootSelection::MsDos, BootSelection::FreeDos] {
                state.select_boot(unsupported);
                assert_eq!(state.model.boot_selection, supported);
            }
        }
    }

    #[test]
    fn selected_iso_label_uses_the_real_filename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("actual-system-image.iso");
        std::fs::write(&path, b"image fixture").unwrap();
        let mut state = State::default();
        state.apply_image(Some(IsoImage::open(path)));
        assert_eq!(
            state.model.boot_selection_label(),
            "actual-system-image.iso"
        );
        assert_eq!(
            state.model.iso.as_ref().unwrap().name,
            "actual-system-image.iso"
        );
    }

    fn assert_theme(theme: &iced::Theme, mode: shadcn_iced::Mode) {
        let expected = shadcn_iced::theme::neutral(mode).tokens();
        let actual = shadcn_iced::tokens(theme);
        assert_eq!(actual.mode, mode);
        assert_eq!(actual.palette, expected.palette);
        assert_eq!(theme.palette().background, expected.palette.background);
        assert_eq!(theme.palette().text, expected.palette.foreground);
        assert_eq!(theme.palette().primary, expected.palette.primary);
    }

    #[test]
    fn native_theme_maps_system_modes_to_neutral_tokens() {
        for (system, expected) in [
            (iced::theme::Mode::Light, shadcn_iced::Mode::Light),
            (iced::theme::Mode::Dark, shadcn_iced::Mode::Dark),
            (iced::theme::Mode::None, shadcn_iced::Mode::Light),
        ] {
            assert_theme(&native_theme(system), expected);
        }
    }

    #[test]
    fn system_theme_switches_both_ways_and_unknown_mode_falls_back_to_light() {
        let mut state = State::default();
        assert_theme(&state.theme, shadcn_iced::Mode::Light);
        for (system, expected) in [
            (iced::theme::Mode::Dark, shadcn_iced::Mode::Dark),
            (iced::theme::Mode::Light, shadcn_iced::Mode::Light),
            (iced::theme::Mode::Dark, shadcn_iced::Mode::Dark),
            (iced::theme::Mode::None, shadcn_iced::Mode::Light),
        ] {
            state.apply_system_theme(system);
            assert_theme(&state.theme, expected);
        }
    }

    #[test]
    fn system_theme_changes_preserve_application_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selected.iso");
        std::fs::write(&path, b"image fixture").unwrap();
        let mut state = State::default();
        state.apply_image(Some(IsoImage::open(path)));
        state.apply_devices(Ok(vec![rustfus_core::devices::DeviceInfo {
            id: "usb-target".into(),
            path: "/dev/usb-target".into(),
            name: "Selected USB disk".into(),
            serial: None,
            size_bytes: 8_000_000_000,
            removable: true,
            read_only: false,
        }]));
        state.model.set_include_fixed(true);
        state.model.partition_scheme = PartitionScheme::Gpt;
        state.model.target_system = TargetSystem::Legacy;
        state.discovery = Discovery::Failed("temporary discovery error".into());
        state.image_error = Some("previous image error".into());
        state.choosing_image = true;
        state.advanced_open = true;
        let model_before = format!("{:?}", state.model);
        let devices_before = state.device_options.clone();
        let targets_before = state.target_options.clone();

        for mode in [
            iced::theme::Mode::Dark,
            iced::theme::Mode::Light,
            iced::theme::Mode::None,
        ] {
            state.apply_system_theme(mode);
            assert_eq!(format!("{:?}", state.model), model_before);
            assert_eq!(state.device_options, devices_before);
            assert_eq!(state.target_options, targets_before);
            assert_eq!(
                state.discovery,
                Discovery::Failed("temporary discovery error".into())
            );
            assert_eq!(state.image_error.as_deref(), Some("previous image error"));
            assert!(state.choosing_image);
            assert!(state.advanced_open);
        }
    }
}
