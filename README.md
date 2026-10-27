# RustFus

RustFus is a native Rust desktop preview for selecting USB disks and describing a future bootable-media operation. It is in active development. The current application does not write disks.

## Current scope

- Native egui/eframe 0.35 desktop UI.
- Rust minimum supported version: 1.92.
- Native USB disk discovery on Linux, macOS, and Windows.
- Live device polling every two seconds, including device name and capacity.
- Device selection includes USB flash media. The `List USB Hard Drives` option also includes fixed USB SSDs and hard drives, while internal disks remain excluded.
- MBR and GPT choices, plus BIOS or UEFI and Legacy target choices. These are preview selections only and do not analyze an ISO.
- Native ISO file picker through rfd 0.17. Selecting an ISO is a path and extension check only, not an image-content validation step.
- MS-DOS and FreeDOS are shown as disabled future entries.
- Format controls and the start action remain disabled until the write engine exists.
- No mocked devices or placeholder device entries are used.

The preview is intentionally non-destructive. It does not open a disk for writing, format media, create partitions, install a bootloader, or inspect ISO contents.

## Supported build targets

The intended native targets are:

| OS | ARM64 | x86_64 | Notes |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04-arm` CI coverage | `ubuntu-24.04` CI coverage | Linux discovery reads sysfs. |
| macOS | `macos-15` CI coverage | `macos-15-intel` CI coverage | Discovery uses Core Foundation and IOKit bindings. |
| Windows | `windows-11-arm` CI coverage | `windows-2025` CI coverage | Discovery uses SetupAPI and `DeviceIoControl` through `windows-sys`. |

The GitHub-hosted runner labels above are the official labels documented by GitHub. The project interprets ARM and x86 in the request as ARM64 and x86_64. 32-bit targets are not tested or promised.

Native OS calls are made from Rust through platform bindings. RustFus has no Rust-to-C++ bridge and does not invoke shell helpers. Some platform bindings and graphics or desktop dependencies may still link to operating-system libraries implemented in C, as expected for native desktop integration.

## Build and test locally

Launch the desktop app:

```text
cargo run --locked
```

Inspect real USB devices without starting the GUI (includes fixed USB disks):

```text
cargo run --no-default-features --example list_devices
```

```text
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release --locked
```

Core-only tests, without the optional GUI feature and its dependencies:

```text
cargo test --no-default-features
```

The GUI is enabled by default. The optional `screenshot` feature enables eframe's OpenGL screenshot hook for local visual inspection. Create the output directory first, then set `EFRAME_SCREENSHOT_TO=artifacts/rustfus-preview.png` and run `cargo run --features screenshot`. The normal app uses wgpu. Generated screenshots are local artifacts and are not committed.

### Validation in this workspace

- 17 native unit tests passed, including selection retention, unplug safety, USB filtering, and ISO metadata/error handling.
- Formatting and Clippy with warnings denied passed.
- Full GUI checks passed for Windows ARM64 and x86_64, and the core checked for Linux x86_64.
- The macOS ARM64 app launched successfully; a real rendered screenshot is in `artifacts/rustfus-preview.png` locally.
- Physical USB hot-plug and native Linux/Windows runtime behavior still require the hardware checks below. No USB disk was attached during the macOS discovery smoke test.

The post-build UI review is recorded in `anti-slop/audit-001-2026-10-27.md`; its numbered follow-ups have not been applied.

### Linux dependencies

The CI installs the native packages needed by eframe's wgpu/accesskit stack and rfd's desktop integration:

```text
libdbus-1-dev libegl1-mesa-dev libgbm-dev libgl1-mesa-dev
libudev-dev libwayland-dev libxkbcommon-dev xorg-dev
```

Package names can vary on other Linux distributions. The dependency is native desktop integration, not a command-line device-discovery helper.

## CI

`.github/workflows/ci.yml` runs the full native checks on Linux ARM64 and x86_64, macOS ARM64 and Intel x86_64, and Windows ARM64 and x86_64. Each native job runs formatting, Clippy with warnings denied, all-target tests, and a locked release build. The resulting raw binary is uploaded as a platform-specific artifact. Artifacts are portable binaries, not installers.

A separate Ubuntu quality job repeats the explicit quality commands and runs `cargo test --no-default-features` to exercise the core-only configuration.

The matrix is configured for the next push. These environments have not been run locally from this checkout, and compilation or runtime behavior on the other operating systems is not claimed until CI and manual hardware checks have completed.

## Native hardware validation checklist

Run this on real hardware after a successful build. Do not treat a clean compile as hardware validation.

- [ ] Open the app, connect a USB disk, and confirm it appears without restarting the app.
- [ ] Unplug the disk while the app is open and confirm it disappears on a later poll.
- [ ] Connect a blank USB disk and confirm its name and capacity are shown once.
- [ ] Connect a multipartition USB disk and confirm it appears once as a disk, not once per partition.
- [ ] Confirm internal disks never appear in the device list.
- [ ] Enable `List USB Hard Drives` and verify a fixed USB SSD or hard drive can appear, while an internal disk still cannot.
- [ ] Open the native ISO picker, choose a real `.iso`, and confirm the selected path is shown without claiming ISO-content validation.
- [ ] Cancel the ISO picker and confirm the existing selection remains unchanged.
- [ ] Try an invalid extension and confirm it is rejected.
- [ ] Confirm MS-DOS and FreeDOS remain disabled.
- [ ] Confirm format controls and the start action remain disabled, and that no disk writing occurs.

## Module structure

- `src/main.rs`: egui/eframe application shell, controls, preview status, and native ISO picker integration.
- `src/model.rs`: boot, partition, target, ISO, and selection state with unit tests.
- `src/devices.rs`: device model, two-second monitor thread, filtering, sorting, and capacity formatting.
- `src/platform/mod.rs`: OS dispatch for device enumeration.
- `src/platform/linux.rs`: sysfs enumeration and USB topology filtering.
- `src/platform/macos.rs`: Core Foundation and IOKit registry enumeration.
- `src/platform/windows.rs`: SetupAPI enumeration and storage queries through `windows-sys`.

No write engine is present yet. The platform modules currently provide discovery data only.

## License

MIT OR Apache-2.0
