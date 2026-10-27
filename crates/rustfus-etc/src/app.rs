use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

use iced::{Element, Subscription, Task};
use rustfus_core::devices::{DeviceInfo, DeviceMonitor};
use rustfus_core::media::{self, Phase, Progress, WriteRequest, WriteSummary};
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
    SelectCluster(Choice<Option<u32>>),
    SelectFilesystem(Choice<String>),
    LabelChanged(String),
    IncludeFixed(bool),
    ChooseImage,
    ImageChosen(Option<Result<IsoImage, String>>),
    Start,
    ConfirmationChanged(String),
    Confirm,
    DismissConfirmation,
    Cancel,
    CopyError,
    EditSettings,
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

struct Worker {
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<Option<Progress>>>,
    done: mpsc::Receiver<Result<WriteSummary, String>>,
}

pub(crate) struct App {
    pub state: State,
    monitor: DeviceMonitor,
    worker: Option<Worker>,
}

pub(crate) struct State {
    pub model: AppModel,
    pub discovery: Discovery,
    pub image_error: Option<String>,
    pub choosing_image: bool,
    pub device_options: Vec<Choice<String>>,
    pub theme: iced::Theme,
    pub volume_label: String,
    pub cluster_size: Option<u32>,
    pub confirmation: Option<WriteRequest>,
    pub confirmation_text: String,
    pub running: bool,
    pub cancelling: bool,
    pub active_request: Option<WriteRequest>,
    pub progress: Option<Progress>,
    pub result: Option<Result<WriteSummary, String>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            model: AppModel::default(),
            discovery: Discovery::Loading,
            image_error: None,
            choosing_image: false,
            device_options: Vec::new(),
            theme: native_theme(iced::theme::Mode::Light),
            volume_label: "RUSTFUS".into(),
            cluster_size: None,
            confirmation: None,
            confirmation_text: String::new(),
            running: false,
            cancelling: false,
            active_request: None,
            progress: None,
            result: None,
        }
    }
}

impl State {
    pub fn locked(&self) -> bool {
        self.running || self.confirmation.is_some() || self.choosing_image
    }

    pub fn apply_devices(&mut self, result: Result<Vec<DeviceInfo>, String>) {
        if self.running || self.confirmation.is_some() {
            return;
        }
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
            self.model.target_system = TargetSystem::BiosOrUefi;
        }
    }

    pub fn apply_image(&mut self, result: Option<Result<IsoImage, String>>) {
        if self.running || self.confirmation.is_some() {
            return;
        }
        self.choosing_image = false;
        match result {
            Some(Ok(image)) => {
                self.model.iso = Some(image);
                self.select_boot(BootSelection::Iso);
                self.image_error = None;
            }
            Some(Err(error)) => self.image_error = Some(error),
            None => {}
        }
    }

    pub fn validation_error(&self) -> Option<String> {
        if self.choosing_image {
            return Some("Finish selecting an ISO first.".into());
        }
        let Some(device) = self.model.selected_device() else {
            return Some("Select a USB disk.".into());
        };
        if device.read_only {
            return Some("The selected disk is read-only.".into());
        }
        if !matches!(
            self.model.boot_selection,
            BootSelection::NonBootable | BootSelection::Iso
        ) {
            return Some("Choose Non Bootable or Windows ISO.".into());
        }
        if self.model.boot_selection == BootSelection::Iso && self.model.iso.is_none() {
            return Some("Select a Windows installer ISO.".into());
        }
        let label = &self.volume_label;
        if label.len() > 11
            || !label.is_ascii()
            || label.starts_with(' ')
            || label.ends_with(' ')
            || label
                .bytes()
                .any(|b| b < 0x20 || b == 0x7f || b"\"*+,./:;<=>?[\\]|".contains(&b))
        {
            return Some(
                "Label: up to 11 ASCII characters, no edge spaces or FAT32 punctuation.".into(),
            );
        }
        None
    }

    fn request_confirmation(&mut self) {
        if self.locked() || self.validation_error().is_some() {
            return;
        }
        let Some(device) = self.model.selected_device().cloned() else {
            return;
        };
        self.confirmation = Some(WriteRequest {
            device,
            boot: self.model.boot_selection,
            image: self.model.iso.clone(),
            scheme: self.model.partition_scheme,
            target: TargetSystem::BiosOrUefi,
            volume_label: self.volume_label.clone(),
            cluster_size: self.cluster_size,
        });
        self.confirmation_text.clear();
        self.result = None;
    }

    pub fn can_confirm(&self) -> bool {
        !self.running
            && self.confirmation.as_ref().is_some_and(|r| {
                !r.device.path.is_empty()
                    && self.confirmation_text == r.device.path
                    && self.model.selected_device().is_some_and(|d| d == &r.device)
            })
    }

    fn begin_confirmed(&mut self) -> Option<(WriteRequest, String)> {
        if !self.can_confirm() {
            return None;
        }
        let request = self.confirmation.take()?;
        self.active_request = Some(request.clone());
        self.running = true;
        self.cancelling = false;
        self.result = None;
        self.progress = Some(Progress {
            phase: Phase::Preparing,
            completed: 0,
            total: 0,
            detail: "Starting background operation".into(),
        });
        Some((request, std::mem::take(&mut self.confirmation_text)))
    }

    fn finish(&mut self, result: Result<WriteSummary, String>) {
        self.running = false;
        self.cancelling = false;
        self.progress = None;
        self.result = Some(result);
    }

    fn configure(&mut self, message: Message) {
        if self.locked() {
            return;
        }
        match message {
            Message::SelectDevice(choice)
                if self.model.devices.iter().any(|d| d.id == choice.value) =>
            {
                self.model.selected_device = Some(choice.value)
            }
            Message::SelectBoot(choice) => self.select_boot(choice.value),
            Message::SelectPartition(scheme) => self.model.partition_scheme = scheme,
            Message::SelectFilesystem(choice) if choice.value == "FAT32" => {}
            Message::SelectTarget(choice) if choice.value == TargetSystem::BiosOrUefi => {
                self.model.target_system = choice.value
            }
            Message::SelectCluster(choice)
                if choice.value.is_none_or(|v| {
                    matches!(v, 512 | 1024 | 2048 | 4096 | 8192 | 16384 | 32768)
                }) =>
            {
                self.cluster_size = choice.value
            }
            Message::LabelChanged(label) => self.volume_label = label,
            Message::IncludeFixed(include) => {
                self.model.set_include_fixed(include);
                self.update_device_options();
            }
            _ => {}
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
            worker: None,
        }
    }

    fn poll_worker(&mut self) {
        let Some(worker) = &self.worker else {
            return;
        };
        if let Ok(mut progress) = worker.progress.lock()
            && let Some(progress) = progress.take()
        {
            self.state.progress = Some(progress);
        }
        let done = match worker.done.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("The media worker stopped unexpectedly. The disk may be incomplete and need reformatting.".into())),
            Err(mpsc::TryRecvError::Empty) => None,
        };
        if let Some(result) = done {
            self.worker = None;
            self.state.finish(result);
            self.monitor.refresh();
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SystemThemeChanged(mode) => self.state.apply_system_theme(mode),
            Message::DeviceUpdate => {
                self.poll_worker();
                if !self.state.running
                    && self.state.confirmation.is_none()
                    && let Some(result) = self.monitor.try_recv()
                {
                    self.state.apply_devices(result);
                }
            }
            Message::ChooseImage if !self.state.locked() => {
                self.state.choosing_image = true;
                return Task::perform(pick_iso(), Message::ImageChosen);
            }
            Message::ImageChosen(result) => self.state.apply_image(result),
            Message::Start => self.state.request_confirmation(),
            Message::EditSettings if !self.state.locked() => self.state.result = None,
            Message::ConfirmationChanged(value)
                if !self.state.running && self.state.confirmation.is_some() =>
            {
                self.state.confirmation_text = value
            }
            Message::DismissConfirmation if !self.state.running => {
                self.state.confirmation = None;
                self.state.confirmation_text.clear();
            }
            Message::Confirm => {
                if let Some((request, confirmation)) = self.state.begin_confirmed() {
                    let cancel = Arc::new(AtomicBool::new(false));
                    let progress = Arc::new(Mutex::new(None));
                    let (send, done) = mpsc::sync_channel(1);
                    let worker_cancel = Arc::clone(&cancel);
                    let worker_progress = Arc::clone(&progress);
                    match std::thread::Builder::new()
                        .name("rustfus-media".into())
                        .spawn(move || {
                            let result =
                                media::run(request, &confirmation, &worker_cancel, |value| {
                                    if let Ok(mut latest) = worker_progress.lock() {
                                        *latest = Some(value);
                                    }
                                });
                            let _ = send.send(result);
                        }) {
                        Ok(_) => {
                            self.worker = Some(Worker {
                                cancel,
                                progress,
                                done,
                            })
                        }
                        Err(error) => self
                            .state
                            .finish(Err(format!("Could not start the media worker: {error}"))),
                    }
                }
            }
            Message::Cancel | Message::Close if self.state.running => {
                if let Some(worker) = &self.worker {
                    worker.cancel.store(true, Ordering::Relaxed);
                }
                self.state.cancelling = true;
            }
            Message::CopyError => {
                if let Some(Err(error)) = &self.state.result {
                    return iced::clipboard::write(error.clone());
                }
                if let Some(error) = &self.state.image_error {
                    return iced::clipboard::write(error.clone());
                }
                if let Discovery::Failed(error) = &self.state.discovery {
                    return iced::clipboard::write(error.clone());
                }
            }
            Message::Close if !self.state.choosing_image => return iced::exit(),
            other => self.state.configure(other),
        }
        Task::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_millis(100)).map(|_| Message::DeviceUpdate),
            iced::system::theme_changes().map(Message::SystemThemeChanged),
            iced::window::close_requests().map(|_| Message::Close),
            iced::event::listen_with(|event, _, _| match event {
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                    ..
                }) => Some(Message::DismissConfirmation),
                _ => None,
            }),
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
        .set_title("Select Windows installer ISO")
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

    fn ready() -> State {
        let mut state = State::default();
        state.apply_devices(Ok(vec![DeviceInfo {
            id: "fixture".into(),
            path: "/dev/NEVER-OPEN-test-fixture".into(),
            name: "Test disk".into(),
            serial: None,
            size_bytes: 8_000_000_000,
            removable: true,
            read_only: false,
        }]));
        state
    }

    #[test]
    fn wrong_confirmation_and_cancel_never_start() {
        let mut state = ready();
        state.request_confirmation();
        state.confirmation_text = "wrong".into();
        assert!(state.begin_confirmed().is_none());
        assert!(!state.running);
        state.confirmation = None;
        assert!(state.begin_confirmed().is_none());
        assert!(state.active_request.is_none());
    }

    #[test]
    fn busy_controls_and_scans_cannot_change_snapshot() {
        let mut state = ready();
        state.request_confirmation();
        let snapshot = format!("{:?}", state.confirmation);
        for running in [false, true] {
            state.running = running;
            state.configure(Message::LabelChanged("CHANGED".into()));
            state.configure(Message::SelectPartition(PartitionScheme::Gpt));
            state.configure(Message::IncludeFixed(true));
            state.configure(Message::SelectBoot(Choice::new(
                BootSelection::Iso,
                "Windows ISO",
            )));
            state.configure(Message::SelectCluster(Choice::new(Some(512), "512")));
            state.apply_devices(Ok(vec![]));
            assert_eq!(format!("{:?}", state.confirmation), snapshot);
            assert_eq!(state.volume_label, "RUSTFUS");
            assert_eq!(state.model.partition_scheme, PartitionScheme::Mbr);
            assert_eq!(state.model.boot_selection, BootSelection::NonBootable);
            assert!(state.model.selected_device().is_some());
            assert_eq!(state.cluster_size, None);
        }
    }

    #[test]
    fn changed_target_denies_confirmation_even_if_text_matches() {
        let mut state = ready();
        state.request_confirmation();
        state.confirmation_text = state.confirmation.as_ref().unwrap().device.path.clone();
        state.model.selected_device = None;
        assert!(!state.can_confirm());
        assert!(state.begin_confirmed().is_none());
    }

    #[test]
    fn success_and_error_finish_progress_without_opening_any_disk() {
        let mut state = ready();
        for success in [true, false] {
            state.request_confirmation();
            state.confirmation_text = state.confirmation.as_ref().unwrap().device.path.clone();
            let (request, _) = state.begin_confirmed().unwrap();
            assert_eq!(request.device.path, "/dev/NEVER-OPEN-test-fixture");
            assert!(state.running);
            assert_eq!(state.progress.as_ref().unwrap().phase, Phase::Preparing);
            state.finish(if success {
                Ok(WriteSummary {
                    files_written: 0,
                    bytes_written: 0,
                    architecture: None,
                })
            } else {
                Err("fixture failure".into())
            });
            assert!(!state.running);
            assert!(state.progress.is_none());
            assert_eq!(state.result.as_ref().unwrap().is_ok(), success);
        }
    }

    #[test]
    fn validation_blocks_read_only_missing_iso_and_invalid_label() {
        let mut state = ready();
        state.volume_label = "bad/name".into();
        state.request_confirmation();
        assert!(state.confirmation.is_none());
        state.volume_label = "VALID".into();
        state.select_boot(BootSelection::Iso);
        state.request_confirmation();
        assert!(state.confirmation.is_none());
        state.select_boot(BootSelection::NonBootable);
        state.model.devices[0].read_only = true;
        state.request_confirmation();
        assert!(state.confirmation.is_none());
    }

    #[test]
    fn discovery_failure_recovers() {
        let mut state = ready();
        state.apply_devices(Err("scan failed".into()));
        assert!(state.model.selected_device().is_none());
        state.apply_devices(Ok(vec![]));
        assert_eq!(state.discovery, Discovery::Ready);
    }

    #[test]
    fn cancelled_and_failed_picker_preserve_previous_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.iso");
        std::fs::write(&path, b"metadata fixture, not an installer").unwrap();
        let mut state = ready();
        state.apply_image(Some(IsoImage::open(path.clone())));
        state.choosing_image = true;
        state.apply_image(None);
        assert_eq!(state.model.iso.as_ref().unwrap().path, path);
        state.apply_image(Some(Err("unreadable".into())));
        assert_eq!(state.model.iso.as_ref().unwrap().path, path);
        assert!(state.image_error.is_some());
    }

    #[test]
    fn unsupported_modes_are_rejected() {
        let mut state = ready();
        for mode in [BootSelection::MsDos, BootSelection::FreeDos] {
            state.select_boot(mode);
        }
        assert_eq!(state.model.boot_selection, BootSelection::NonBootable);
        state.configure(Message::SelectTarget(Choice::new(
            TargetSystem::Legacy,
            "Legacy",
        )));
        assert_eq!(state.model.target_system, TargetSystem::BiosOrUefi);
    }

    #[test]
    fn system_themes_preserve_busy_application_state() {
        let mut state = ready();
        state.request_confirmation();
        let snapshot = format!("{:?}", state.confirmation);
        for (system, expected) in [
            (iced::theme::Mode::Dark, shadcn_iced::Mode::Dark),
            (iced::theme::Mode::Light, shadcn_iced::Mode::Light),
            (iced::theme::Mode::None, shadcn_iced::Mode::Light),
        ] {
            state.apply_system_theme(system);
            assert_eq!(
                shadcn_iced::tokens(&state.theme).palette,
                shadcn_iced::theme::neutral(expected).tokens().palette
            );
            assert_eq!(format!("{:?}", state.confirmation), snapshot);
        }
    }
}
