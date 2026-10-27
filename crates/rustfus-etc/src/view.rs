use iced::widget::{column, container, row, rule, scrollable, space, text};
use iced::{Element, Length};
use rustfus_core::devices::format_bytes;
use rustfus_core::model::{BootSelection, PartitionScheme};
use shadcn_iced::{button, checkbox, collapsible, input, radio_group, select};

use crate::app::{Discovery, Message, State};
use crate::components::{Choice, field, note, section};

pub(crate) fn view(state: &State) -> Element<'_, Message> {
    let form = column![
        drive_properties(state),
        format_options(state),
        status(state)
    ]
    .spacing(24)
    .width(Length::Fill);
    let footer = column![
        rule::horizontal(1),
        row![
            note("This build never modifies disks.", &state.theme),
            space::horizontal(),
            button::button("START").disabled(true).width(100),
            button::button("CLOSE")
                .on_press(Message::Close)
                .variant(button::Variant::Outline)
                .width(100),
        ]
        .spacing(10)
        .align_y(iced::Center),
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
                &state.theme,
            ),
            space::horizontal(),
            note(concat!("RustFus ", env!("CARGO_PKG_VERSION")), &state.theme),
        ],
    ]
    .spacing(12);
    container(column![scrollable(form).height(Length::Fill), footer].spacing(18))
        .padding(24)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn drive_properties(state: &State) -> Element<'_, Message> {
    let device = state.model.selected_device.as_ref().and_then(|id| {
        state
            .device_options
            .iter()
            .find(|choice| &choice.value == id)
    });
    let placeholder = match state.discovery {
        Discovery::Loading => "Looking for USB disks…",
        Discovery::Failed(_) => "Device discovery unavailable",
        Discovery::Ready if state.device_options.is_empty() => "No USB disks detected",
        Discovery::Ready => "Select a USB disk",
    };
    let mut devices = select::select(device.cloned(), Message::SelectDevice)
        .placeholder(placeholder)
        .disabled(state.device_options.is_empty())
        .width(Length::Fill);
    for choice in &state.device_options {
        devices = devices.option(choice.clone(), choice.label.clone());
    }
    let mut fields = column![field("Device", devices)].spacing(14);
    if let Discovery::Failed(error) = &state.discovery {
        fields = fields.push(
            text(format!("Cannot read USB devices: {error}"))
                .size(12)
                .color(shadcn_iced::tokens(&state.theme).palette.destructive),
        );
    }

    let selected_boot = Choice::new(
        state.model.boot_selection,
        state.model.boot_selection_label(),
    );
    let image_label = state
        .model
        .iso
        .as_ref()
        .map_or("Disk or ISO image (select a file)", |image| {
            image.name.as_str()
        });
    let boot = select::select(Some(selected_boot), Message::SelectBoot)
        .placeholder("Choose boot selection")
        .width(Length::Fill)
        .option(
            Choice::new(BootSelection::NonBootable, "Non Bootable"),
            "Non Bootable",
        )
        .disabled_option(
            Choice::new(BootSelection::MsDos, "MS-DOS"),
            "MS-DOS (not implemented)",
        )
        .disabled_option(
            Choice::new(BootSelection::FreeDos, "FreeDOS"),
            "FreeDOS (not implemented)",
        )
        .option(Choice::new(BootSelection::Iso, image_label), image_label);
    fields = fields.push(field(
        "Boot selection",
        row![
            boot,
            button::button(if state.choosing_image {
                "SELECTING…"
            } else {
                "SELECT"
            })
            .on_press_maybe((!state.choosing_image).then_some(Message::ChooseImage))
            .variant(button::Variant::Outline)
            .width(112),
        ]
        .spacing(10)
        .align_y(iced::Center),
    ));
    if let Some(error) = &state.image_error {
        fields = fields.push(
            text(error)
                .size(12)
                .color(shadcn_iced::tokens(&state.theme).palette.destructive),
        );
    }
    if state.model.boot_selection == BootSelection::Iso {
        if let Some(image) = &state.model.iso {
            fields = fields.push(note(
                format!(
                    "{} · {}",
                    format_bytes(image.size_bytes),
                    image.path.display()
                ),
                &state.theme,
            ));
            fields = fields.push(note(
                "Image contents and boot compatibility have not been checked.",
                &state.theme,
            ));
        } else {
            fields = fields.push(note("Choose SELECT to pick an ISO image.", &state.theme));
        }
    }

    let partition =
        radio_group::radio_group(Some(state.model.partition_scheme), Message::SelectPartition)
            .option(PartitionScheme::Mbr, "MBR")
            .option(PartitionScheme::Gpt, "GPT")
            .orientation(radio_group::Orientation::Horizontal);
    let selected_target = state
        .target_options
        .iter()
        .find(|choice| choice.value == state.model.target_system)
        .cloned();
    let mut target = select::select(selected_target, Message::SelectTarget)
        .placeholder("Target system")
        .width(Length::Fill);
    for choice in &state.target_options {
        target = target.option(choice.clone(), choice.label.clone());
    }
    fields = fields.push(
        row![
            field(
                "Partition scheme",
                container(partition).height(36).align_y(iced::Center)
            ),
            field("Target system", target),
        ]
        .spacing(24),
    );
    fields = fields.push(note(
        "Partition and target choices are configuration only in this preview.",
        &state.theme,
    ));

    let advanced = column![
        checkbox::checkbox(state.model.include_usb_hard_drives)
            .label("List USB Hard Drives")
            .description(
                "Include fixed USB SSDs and hard drives. Internal disks are always excluded."
            )
            .on_toggle(Message::IncludeFixed),
        checkbox::checkbox(false)
            .label("Add fixes for old BIOSes (extra partition, align, etc.)")
            .description("BIOS compatibility fixes are not implemented.")
            .disabled(true),
    ]
    .spacing(10);
    fields = fields.push(
        collapsible::collapsible("Advanced drive properties", advanced)
            .open(state.advanced_open)
            .on_toggle(|_| Message::ToggleAdvanced),
    );
    section("Drive Properties", fields)
}

fn format_options(state: &State) -> Element<'_, Message> {
    let disabled_select = || {
        select::select(None::<Choice<String>>, Message::SelectDevice)
            .placeholder("Not implemented")
            .disabled(true)
            .width(Length::Fill)
    };
    section(
        "Format Options",
        column![
            field(
                "Volume label",
                input::input("Not available in this preview", "").width(Length::Fill)
            ),
            row![
                field("File system", disabled_select()),
                field("Cluster size", disabled_select()),
            ]
            .spacing(24),
            note(
                "Formatting and BIOS compatibility fixes will be enabled with the write engine.",
                &state.theme
            ),
        ]
        .spacing(14),
    )
}

fn status(state: &State) -> Element<'_, Message> {
    let (title, description) = match &state.discovery {
        Discovery::Loading => (
            "SCANNING FOR USB DISKS",
            "Discovery runs in the background.",
        ),
        Discovery::Failed(_) => (
            "DEVICE SCAN FAILED",
            "Discovery will retry automatically. No target is selected.",
        ),
        Discovery::Ready if state.model.devices.is_empty() => (
            "WAITING FOR A USB DISK",
            "Connect a USB drive. For fixed USB SSDs, enable List USB Hard Drives.",
        ),
        Discovery::Ready if state.model.selected_device().is_none() => {
            ("SELECT A DEVICE", "Choose a USB disk from the Device menu.")
        }
        Discovery::Ready
            if state
                .model
                .selected_device()
                .is_some_and(|device| device.read_only) =>
        {
            (
                "READ-ONLY DEVICE",
                "The operating system reports this USB disk as write-protected.",
            )
        }
        Discovery::Ready => (
            "PREVIEW ONLY",
            "Selections are available. Writing bootable media is not implemented.",
        ),
    };
    section(
        "Status",
        column![
            container(text(title).size(14))
                .padding(13)
                .center_x(Length::Fill)
                .style(|theme: &iced::Theme| container::Style {
                    background: Some(shadcn_iced::tokens(theme).palette.muted.into()),
                    border: iced::Border {
                        width: 1.0,
                        color: shadcn_iced::tokens(theme).palette.border,
                        radius: shadcn_iced::tokens(theme).radius_sm().into(),
                    },
                    ..Default::default()
                }),
            note(description, &state.theme),
        ]
        .spacing(10),
    )
}
