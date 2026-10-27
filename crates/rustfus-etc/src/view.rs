use crate::app::{Discovery, Message, State};
use crate::components::{Choice, field, note, section};
use iced::widget::{column, container, row, rule, space, text};
use iced::{Element, Length};
use rustfus_core::devices::format_bytes;
use rustfus_core::media::Phase;
use rustfus_core::model::{BootSelection, PartitionScheme, TargetSystem};
use shadcn_iced::{button, checkbox, input, progress, radio_group, select};

pub(crate) fn view(state: &State) -> Element<'_, Message> {
    let busy = state.running || state.confirmation.is_some();
    let mut body = column![
        drive_properties(state),
        format_options(state),
        status(state)
    ]
    .spacing(12)
    .width(Length::Fill);
    if state.running || state.result.is_some() {
        body = column![status(state)].spacing(12);
    }
    if let Some(request) = &state.confirmation {
        let warning = format!(
            "This will erase all partitions on {}. Type the exact path to confirm.",
            request.device.path
        );
        body = column![section(
            "Confirm destructive write",
            column![
                text(short_name(&request.device.name)).size(14),
                note(
                    format!(
                        "{} · {:?} · FAT32",
                        format_bytes(request.device.size_bytes),
                        request.scheme
                    ),
                    &state.theme
                ),
                note(warning, &state.theme),
                field(
                    "Device path",
                    input::input("/dev/...", &state.confirmation_text)
                        .on_input_maybe((!state.running).then_some(Message::ConfirmationChanged))
                ),
                row![
                    button::button("Cancel")
                        .on_press_maybe((!state.running).then_some(Message::DismissConfirmation))
                        .variant(button::Variant::Outline),
                    space::horizontal(),
                    button::button("Erase and write")
                        .on_press_maybe(state.can_confirm().then_some(Message::Confirm))
                ]
                .spacing(8)
            ]
            .spacing(7),
        )];
    }
    let footer = column![
        rule::horizontal(1),
        row![
            note(
                format!(
                    "{} USB device{} found",
                    state.model.devices.len(),
                    if state.model.devices.len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                ),
                &state.theme
            ),
            space::horizontal(),
            button::button(if state.running {
                "Cancel"
            } else {
                "Write media"
            })
            .on_press_maybe(if state.running {
                (!state.cancelling).then_some(Message::Cancel)
            } else {
                (!state.locked() && state.validation_error().is_none()).then_some(Message::Start)
            })
            .variant(if state.running {
                button::Variant::Destructive
            } else {
                button::Variant::Default
            }),
            button::button("Close")
                .on_press_maybe((!state.running && !state.choosing_image).then_some(Message::Close))
                .variant(button::Variant::Outline)
        ]
        .spacing(8)
        .align_y(iced::Center),
        row![
            note(
                format!("RustFus {}", env!("CARGO_PKG_VERSION")),
                &state.theme
            ),
            space::horizontal(),
            note(
                if busy {
                    "Controls locked during write"
                } else {
                    "NonBootable FAT32 or Windows ISO"
                },
                &state.theme
            )
        ]
        .spacing(8)
    ]
    .spacing(7);
    container(column![body, footer].spacing(12))
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn drive_properties(state: &State) -> Element<'_, Message> {
    let selected = state
        .model
        .selected_device
        .as_ref()
        .and_then(|id| state.device_options.iter().find(|c| &c.value == id))
        .cloned();
    let placeholder = match state.discovery {
        Discovery::Loading => "Scanning USB disks...",
        Discovery::Failed(_) => "Device discovery unavailable",
        Discovery::Ready if state.device_options.is_empty() => "No USB disks detected",
        Discovery::Ready => "Select a USB disk",
    };
    let mut devices = select::select(selected, Message::SelectDevice)
        .placeholder(placeholder)
        .disabled(state.locked() || state.device_options.is_empty())
        .width(Length::Fill);
    for choice in &state.device_options {
        devices = devices.option(choice.clone(), choice.label.clone());
    }
    let mut fields = column![field("Device", devices)].spacing(7);
    if let Discovery::Failed(error) = &state.discovery {
        fields = fields.push(note(
            format!("Cannot read USB devices: {}", short(error)),
            &state.theme,
        ));
    }
    let selected_boot = Choice::new(
        state.model.boot_selection,
        if state.model.boot_selection == BootSelection::Iso {
            "Windows ISO"
        } else {
            "Non Bootable"
        },
    );
    let boot = select::select(Some(selected_boot), Message::SelectBoot)
        .disabled(state.locked())
        .width(Length::Fill)
        .option(
            Choice::new(BootSelection::NonBootable, "Non Bootable"),
            "Non Bootable",
        )
        .option(
            Choice::new(
                BootSelection::Iso,
                state
                    .model
                    .iso
                    .as_ref()
                    .map_or("Windows ISO".into(), |i| i.name.clone()),
            ),
            "Windows ISO",
        );
    fields = fields.push(field(
        "Boot options",
        row![
            boot,
            button::button(if state.choosing_image {
                "SELECTING..."
            } else {
                "Select ISO"
            })
            .on_press_maybe((!state.locked()).then_some(Message::ChooseImage))
            .variant(button::Variant::Outline)
        ]
        .spacing(7)
        .align_y(iced::Center),
    ));
    if state.model.boot_selection == BootSelection::Iso {
        fields = fields.push(note(
            state
                .model
                .iso
                .as_ref()
                .map_or("Select a Windows installer ISO.".into(), |i| {
                    format!("{} · {}", short_name(&i.name), format_bytes(i.size_bytes))
                }),
            &state.theme,
        ));
    }
    let partition =
        radio_group::radio_group(Some(state.model.partition_scheme), Message::SelectPartition)
            .option(PartitionScheme::Mbr, "MBR")
            .option(PartitionScheme::Gpt, "GPT")
            .orientation(radio_group::Orientation::Horizontal)
            .disabled(state.locked());
    let target = select::select(
        Some(Choice::new(TargetSystem::BiosOrUefi, "UEFI")),
        Message::SelectTarget,
    )
    .disabled(state.locked() || state.model.boot_selection != BootSelection::Iso)
    .width(Length::Fill)
    .option(Choice::new(TargetSystem::BiosOrUefi, "UEFI"), "UEFI");
    fields = fields.push(
        row![
            field("Partition scheme", partition),
            field("ISO target", target)
        ]
        .spacing(12),
    );
    fields = fields.push(
        checkbox::checkbox(state.model.include_usb_hard_drives)
            .label("Include fixed USB disks")
            .description("Internal disks remain excluded.")
            .on_toggle(Message::IncludeFixed)
            .disabled(state.locked()),
    );
    section("Drive", fields)
}

fn format_options(state: &State) -> Element<'_, Message> {
    let cluster_choices = [
        (None, "Auto"),
        (Some(512), "512 B"),
        (Some(1024), "1 KiB"),
        (Some(2048), "2 KiB"),
        (Some(4096), "4 KiB"),
        (Some(8192), "8 KiB"),
        (Some(16384), "16 KiB"),
        (Some(32768), "32 KiB"),
    ];
    let mut cluster = select::select(
        Some(Choice::new(
            state.cluster_size,
            cluster_choices
                .iter()
                .find(|(v, _)| *v == state.cluster_size)
                .map_or("Auto", |(_, l)| *l),
        )),
        Message::SelectCluster,
    )
    .disabled(state.locked())
    .width(Length::Fill);
    for (value, label) in cluster_choices {
        cluster = cluster.option(Choice::new(value, label), label);
    }
    section(
        "Format",
        column![
            row![
                field(
                    "Volume label",
                    input::input("FAT32 label", &state.volume_label)
                        .on_input_maybe((!state.locked()).then_some(Message::LabelChanged))
                        .width(Length::Fill)
                ),
                field(
                    "File system",
                    select::select(
                        Some(Choice::new("FAT32".to_owned(), "FAT32")),
                        Message::SelectFilesystem
                    )
                    .option(Choice::new("FAT32".to_owned(), "FAT32"), "FAT32")
                    .disabled(state.locked())
                    .width(Length::Fill)
                ),
                field("Cluster size", cluster)
            ]
            .spacing(10),
            note(
                "FAT32, maximum 4 GiB per file. Windows installer WIM files are split when needed.",
                &state.theme
            )
        ]
        .spacing(7),
    )
}

fn status(state: &State) -> Element<'_, Message> {
    if !state.running
        && state.result.is_none()
        && let Some(error) = &state.image_error
    {
        return section(
            "ISO selection failed",
            column![
                note(short(error), &state.theme),
                button::button("Copy error details")
                    .on_press(Message::CopyError)
                    .variant(button::Variant::Outline)
            ]
            .spacing(5),
        );
    }
    if let Some(progress) = &state.progress {
        let fraction = if progress.total == 0 {
            0.0
        } else {
            (progress.completed as f32 / progress.total as f32).clamp(0.0, 1.0)
        };
        return section(
            "Writing",
            column![
                note(
                    state
                        .active_request
                        .as_ref()
                        .map_or(String::new(), |r| format!(
                            "Target: {} · {}",
                            short_name(&r.device.name),
                            r.device.path
                        )),
                    &state.theme
                ),
                text(if progress.total == 0 {
                    phase_label(progress.phase).to_owned()
                } else {
                    format!("{} · {:.0}%", phase_label(progress.phase), fraction * 100.0)
                })
                .size(14),
                progress::progress(fraction * 100.0),
                note(
                    if state.cancelling {
                        "Cancelling. The media may be incomplete and need reformatting."
                    } else {
                        progress.detail.as_str()
                    },
                    &state.theme
                )
            ]
            .spacing(7),
        );
    }
    if let Some(result) = &state.result {
        return section(
            "Result",
            column![
                text(if result.is_ok() {
                    "Media write complete"
                } else {
                    "Media write failed"
                })
                .size(14),
                note(
                    result.as_ref().err().map_or_else(
                        || result.as_ref().ok().map_or(String::new(), |s| format!(
                            "Verified {} files, {} written{}.",
                            s.files_written,
                            format_bytes(s.bytes_written),
                            s.architecture
                                .as_ref()
                                .map_or(String::new(), |a| format!(", {a} UEFI"))
                        )),
                        |e| short(e)
                    ),
                    &state.theme
                ),
                button::button("Copy error details")
                    .on_press_maybe(result.as_ref().err().map(|_| Message::CopyError))
                    .variant(button::Variant::Outline),
                button::button("Edit settings")
                    .on_press(Message::EditSettings)
                    .variant(button::Variant::Outline)
            ]
            .spacing(7),
        );
    }
    let (title, detail): (&str, String) = match &state.discovery {
        Discovery::Loading => (
            "Scanning",
            "Device discovery runs in the background.".into(),
        ),
        Discovery::Failed(error) => ("Device scan failed", short(error)),
        Discovery::Ready if state.model.devices.is_empty() => {
            ("Waiting for a USB disk", "Connect a USB drive.".into())
        }
        Discovery::Ready if state.model.selected_device().is_none() => {
            ("Select a device", "Choose a USB disk.".into())
        }
        Discovery::Ready if state.model.selected_device().is_some_and(|d| d.read_only) => (
            "Read-only device",
            "The operating system reports write protection.".into(),
        ),
        Discovery::Ready => (
            "Ready to write",
            state
                .validation_error()
                .unwrap_or_else(|| "All data on the selected disk will be erased.".into()),
        ),
    };
    section(
        "Status",
        column![text(title).size(14), note(detail, &state.theme)].spacing(5),
    )
}
fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::Preparing => "Preparing",
        Phase::Opening => "Opening device",
        Phase::Partitioning => "Partitioning",
        Phase::Formatting => "Formatting FAT32",
        Phase::Copying => "Copying files",
        Phase::Verifying => "Verifying",
        Phase::Flushing => "Flushing device",
    }
}
fn short(value: &str) -> String {
    let mut s = value.chars().take(240).collect::<String>();
    if value.chars().count() > 240 {
        s.push_str("...");
    }
    s
}

fn short_name(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 36 {
        return value.to_owned();
    }
    format!(
        "{}...{}",
        chars[..18].iter().collect::<String>(),
        chars[chars.len() - 15..].iter().collect::<String>()
    )
}
