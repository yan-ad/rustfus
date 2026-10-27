# RustFus

RustFus is a native Rust desktop preview for selecting USB disks and describing a future bootable-media operation. It is in active development and does not write disks.

## Workspace migration

The workspace is implemented with iced and the linked r4ai `shadcn-iced` component library.

The root is a virtual Cargo workspace with two members:

| Package | Path | Responsibility |
| --- | --- | --- |
| `rustfus-core` | `crates/rustfus-core` | GUI-independent device discovery, models, platform bindings, unit tests, and the `list_devices` diagnostic example. |
| `rustfus-etc` | `crates/rustfus-etc` | Native iced application and asynchronous ISO picker. Provides the `rustfus` binary and is the default workspace member. |

Rust 1.92 is required. UI dependencies are pinned to `iced = "=0.14.0"` and [`shadcn-iced = "=0.3.0"`](https://shadcn-iced.r4ai.dev/docs/installation). Buttons, selects, radio groups, checkboxes, inputs, and collapsible controls come from that component library. The app uses native wgpu/tiny-skia rendering, not wry or browser features.

## Preview scope

- Native USB disk discovery on Linux, macOS, and Windows.
- Automatic background discovery every two seconds, including device name and capacity. There is no refresh button or timing label.
- Device selection includes USB flash media. The `List USB Hard Drives` option also includes fixed USB SSDs and hard drives, while internal disks remain excluded.
- MBR and GPT choices, plus BIOS or UEFI and Legacy target choices. These are preview selections only and do not analyze an ISO.
- Asynchronous native ISO file picker through rfd 0.17. Selection validates the `.iso` extension and that the path is a readable, nonempty regular file. It does not validate image contents.
- MS-DOS and FreeDOS are disabled future entries. Format controls and the start action remain disabled until a write engine exists.
- No mocked devices or placeholder device entries.
- System text fonts resolved through Core Text on macOS, Windows `NONCLIENTMETRICS`, and fontconfig on Linux. No application text font is embedded.
- Light/dark colors follow the operating system at startup and through iced's system-theme subscription. There is no in-app theme preference. When the OS reports no preference, light mode is used.
- Geist/Inter font features are disabled. Shadcn's text-font registry uses the resolved system family; only the component library's Lucide icon font is loaded.

The preview is intentionally non-destructive. It does not open a disk for writing, format media, create partitions, install a bootloader, or inspect ISO contents. The migration does not enable writes.

## Supported build targets

The intended native targets are:

| OS | ARM64 | x86_64 | Notes |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04-arm` | `ubuntu-24.04` | Discovery reads sysfs using the standard library. |
| macOS | `macos-15` | `macos-15-intel` | Discovery uses Core Foundation and IOKit bindings. |
| Windows | `windows-11-arm` | `windows-2025` | Discovery uses SetupAPI and `DeviceIoControl` through `windows-sys`. |

These are the official GitHub-hosted runner labels used by CI. ARM and x86 here mean ARM64 and x86_64, respectively. 32-bit targets are unsupported. CI results for the migrated workspace and physical hardware validation are still pending.

Native OS calls are made from Rust through platform bindings. RustFus has no Rust-to-C++ bridge and does not invoke shell helpers. Platform bindings and graphics or desktop dependencies may still link to operating-system libraries implemented in C.

## Build and test locally

Launch the default workspace member:

```text
cargo run --locked
```

Or select the package and binary explicitly:

```text
cargo run --locked -p rustfus-etc --bin rustfus
```

Inspect real USB devices without starting or depending on the GUI (includes fixed USB disks):

```text
cargo run --locked -p rustfus-core --example list_devices
```

Run the full workspace checks and build the application:

```text
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build -p rustfus-etc --bin rustfus --release --locked
```

Core-only checks do not compile GUI dependencies:

```text
cargo fmt -p rustfus-core -- --check
cargo check -p rustfus-core --all-targets
cargo test -p rustfus-core
```

### Linux dependencies

The native GUI jobs install the following Ubuntu packages for fontconfig 0.10, font rendering, wgpu/tiny-skia graphics, windowing, and desktop integration:

```text
libdbus-1-dev libegl1-mesa-dev libfontconfig1-dev libfreetype6-dev
libgbm-dev libgl1-mesa-dev libudev-dev libwayland-dev libxkbcommon-dev
pkg-config xorg-dev
```

At runtime, install the appropriate graphics drivers and libraries, including libxkbcommon, Wayland/EGL where used, and a Vulkan loader such as `libvulkan1` for the Vulkan backend. A machine without a usable GPU can use software Mesa (for example, `mesa-vulkan-drivers` with lavapipe). Package names vary between distributions; compiling successfully does not prove that a display or graphics backend is available.

Core-only Linux checks need none of these GUI development packages. Device discovery uses standard-library sysfs access, not a shell helper or a graphics stack.

## CI

`.github/workflows/ci.yml` preserves six native jobs: Linux ARM64 and x86_64, macOS ARM64 and Intel x86_64, and Windows ARM64 and x86_64. Each runs full workspace formatting, all-target workspace tests, and all-target workspace Clippy with warnings denied, followed by:

```text
cargo build -p rustfus-etc --bin rustfus --release --locked
```

Each job uploads the raw `rustfus` or `rustfus.exe` release binary as a platform-specific artifact. These are not installers or self-contained bundles; native runtime libraries may still be required.

A separate Ubuntu quality job runs only core formatting, checking, and tests using the commands above. It does not install Linux GUI dependencies or build `rustfus-etc`.

### Validation status

- 29 unique tests pass on macOS ARM64: 17 core tests and 12 desktop tests. The screenshot example also runs the same desktop tests.
- Workspace formatting and Clippy with warnings denied pass.
- Full workspace checks pass for Windows ARM64 and x86_64; core checks pass for Linux x86_64. Native Windows/Linux runtime checks and release CI are still pending.
- A real macOS capture is available locally at `artifacts/rustfus-iced-preview.png`. It shows the detected UC300 USB disk and the OS dark theme; no device fixtures are injected.
- Native system-font resolution and shadcn text-family configuration are tested. Dependency features enable Lucide only, not Geist/Inter.
- Light/dark mapping, repeated changes, and state preservation are tested. Manually changing the OS theme while the app is open still needs platform hardware validation.

Capture the running interface using `cargo run -p rustfus-etc --example screenshot`. The old `audit-001` applies only to egui; the iced post-build review is `anti-slop/audit-002-2026-10-27.md`.

## Native hardware validation checklist

Run this on real hardware after a successful build. Do not treat a clean compile as hardware validation. These checks are pending on each supported platform and architecture.

- [ ] Open the app, connect a USB disk, and confirm it appears automatically without restarting or manually refreshing.
- [ ] Unplug the disk while the app is open and confirm it disappears on a later poll.
- [ ] Connect a blank USB disk and confirm its name and capacity are shown once.
- [ ] Connect a multipartition USB disk and confirm it appears once as a disk, not once per partition.
- [ ] Confirm internal disks never appear in the device list.
- [ ] Enable `List USB Hard Drives` and verify a fixed USB SSD or hard drive can appear, while an internal disk still cannot.
- [ ] Open the asynchronous native ISO picker, choose a readable, nonempty regular `.iso` file, and confirm the selected path is shown without claiming ISO-content validation.
- [ ] Cancel the ISO picker and confirm the existing selection remains unchanged.
- [ ] Confirm invalid extensions, unreadable paths, empty files, and non-regular files are rejected.
- [ ] Confirm the app remains responsive during discovery and file selection, with no refresh button or timing label.
- [ ] Confirm system text font resolution works without an embedded application text font.
- [ ] Change the OS light/dark preference while the app is open and confirm all components update without losing device or image selections; no app theme setting should exist.
- [ ] Confirm MS-DOS and FreeDOS remain disabled.
- [ ] Confirm format controls and the start action remain disabled, and that no disk writing occurs.

## License

MIT OR Apache-2.0
