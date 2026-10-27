# RustFus

RustFus is a native Rust desktop application for destructive USB FAT32 quick formatting and a limited Windows UEFI installer-media workflow. It is in active development. **It writes disks and destroys existing partitions.** Use only backed-up, disposable USB media.

Physical formatting, boot, and Secure Boot validation remain pending. The agent has not performed raw hardware writes. See the [full manual media checklist](docs/MANUAL_TESTING.md) before trying a physical disk.

See the [Rufus feature-gap audit](docs/RUFUS_FEATURE_GAPS.md) for the remaining functionality and [system-font rendering notes](docs/SYSTEM_FONT_RENDERING.md) for the narrow macOS optical-sizing correction. The app uses the installed OS text font, with sentence-case action labels.

## Current functional scope

| Mode | What it does | Limits |
| --- | --- | --- |
| Non Bootable | Creates one aligned FAT32 partition using MBR or GPT, then verifies the new filesystem | Quick format only; not a secure erase or BIOS bootloader installation |
| Windows ISO | Stages supported standard Windows installer ISO files, creates FAT32 media, copies source UEFI files, and verifies copied files with SHA-256 | UEFI only; bounded ISO9660/Joliet and physical-partition UDF subset; trusted standard images only |
| Oversized `sources/install.wim` | Natively splits eligible standard v1.13 WIMs into `install.swm`, `install2.swm`, and subsequent parts before opening the target | No compression, XPRESS, or LZX with standard framing; not a general WIM converter or content verifier |

The format target is **FAT32 only**, not NTFS or exFAT. Supported logical sector sizes are 512 and 4096 bytes, subject to actual FAT32 geometry. MBR and GPT layouts use a 1 MiB-aligned partition. GPT Windows ISO output uses an EFI System Partition; GPT non-bootable output uses Microsoft Basic Data. MBR output is an inactive FAT32 LBA partition and has no BIOS loader.

Windows x86, x86_64, and ARM64 UEFI boot-file architectures are recognized when the corresponding valid PE files are present in the ISO. **ISO architecture does not need to match the host architecture.** Booting still requires firmware compatible with the source EFI architecture.

RustFus copies the source Windows EFI files without installing a chainloader. Official Microsoft-signed source files retain their bytes; RustFus does not authenticate the ISO, validate their certificate chains, or guarantee acceptance by Secure Boot firmware. Independently verify the official ISO SHA-256 before use. Reaching Windows Setup does not guarantee installation success or correct installation-target selection.

### Not supported

- MS-DOS, FreeDOS, Legacy BIOS, old BIOS, or BIOS-only installers. DOS and Legacy choices have been removed from the UI; retained core model variants are rejected for boot-media operations.
- NTFS or exFAT formatting, arbitrary ISO/raw-image flashing, or Linux installer-media creation.
- Secure erase, bad-block scanning, full-capacity media certification, or rollback after a destructive write starts.
- Arbitrary/custom/untrusted Windows images. Required standard files include `setup.exe`, `sources/boot.wim`, an install image, and a supported EFI boot file.
- Fragmented, sparse, embedded, continuation, or other unsupported UDF allocations; ISO9660 multi-extent/interleaved entries; unsupported partition maps or filesystems. Broken recognized UDF does not fall back to a potentially truncated ISO bridge tree.
- `install.esd` above FAT32's maximum file length (4 GiB minus one byte). It is rejected before erase, not converted. Smaller ESD files are copied, not decompressed or authenticated.
- Solid/LZMS, pipable, delta, incomplete, already split, unknown-flag, or nonstandard WIM inputs to the splitter. An indivisible encoded resource or metadata set that cannot fit a split part is rejected before erase.

Existing sequential `install.swm`, `install2.swm`, etc. in an ISO can be copied after filename, sequence, size, and filesystem checks. That is distinct from accepting an already-split WIM as input to the splitter. The native splitter preserves encoded resources and existing SHA-1 fields; it does not decompress bitstreams, authenticate WIM resources, or validate Windows image metadata trees. Use trusted source images.

## Safety and native disk access

1. Back up the spare target and unplug unrelated external disks.
2. Compare its **whole-disk path and capacity twice**: in RustFus and in the OS disk utility. Record its identity privately. Serial numbers may be missing, duplicated, or zero-filled; a path or drive number may be reused after reconnection.
3. Close applications using the disk. On Linux and macOS, manually unmount every target volume, **but do not eject the disk**.
4. Select Non Bootable or a trusted Windows ISO, MBR/GPT, a valid FAT32 label, and cluster size.
5. Click `WRITE MEDIA`, read the warning, and type the exact selected device path. `ERASE AND WRITE` starts the destructive workflow.
6. Keep the disk connected until copying, verification, flushing, and native cleanup finish. After cancellation or a partial write, treat the media as incomplete and reformat it before use.

The worker stages source files, splits eligible oversized WIMs, and checks capacity before modifying partition tables. It revalidates device identity and geometry, holds native access guards through cleanup, quick-formats FAT32, synchronizes, reopens the filesystem, and checks file lengths and SHA-256. These checks are not a universal system-disk safety guarantee. Quick format leaves recoverable old data in areas it does not overwrite.

| Host | Permission and behavior | Intentional limitations |
| --- | --- | --- |
| Linux | Root/raw-device permission required. Native whole-device exclusive claim; refuses mounted partitions, active swap, and dm/md holders; sync and partition reread | Does not unmount volumes or elevate itself; namespace and privileged system configuration still require operator care |
| macOS | Root/raw-disk permission required. Disk Arbitration claim and mount veto; native raw whole-disk access and cache synchronization | Mounted volumes refused. APFS, HFS, CoreStorage, Apple backing stores, and unknown content types refused even when unmounted |
| Windows | Administrator rights required. Native whole-disk access, volume locks/dismounts, disk validation, flush, and property refresh | Refuses running Windows disk, ESP/recovery/service/platform-required/dynamic partitions and multi-disk volumes; incomplete or unavailable volume information fails closed |

RustFus does not automatically elevate privileges or invoke shell/helper scripts for disk operations. A permission failure must be addressed by deliberately launching the built application with appropriate privileges, not by bypassing guards.

**Windows reformat limitation:** an existing GPT Windows USB with an EFI System Partition can be refused, including media previously created by RustFus. An unrelated unavailable volume can also block an otherwise eligible target because Windows volume inspection is conservative. Do not bypass these checks or delete partitions on an uncertain disk. Record the refusal and use a different confirmed spare target; disk-management changes are outside this guide.

Cancellation before destructive writes should preserve the disk; cancellation during or after partitioning may leave unusable media. A running window-close request asks for cancellation and keeps the app open for cleanup. Settings are locked while confirming or writing. Neither cleanup nor exclusive access is a promise against every OS automount, driver fault, unplug, or power loss.

## Desktop interface

- Compact 620 x 640 default and minimum window, with no scrolling form or oversized decorative title. Progress and results replace the settings view.
- Native USB discovery in the background, approximately every two seconds. No mocked devices or manual refresh button.
- `Include fixed USB disks` can expose eligible USB SSDs/HDDs. Internal disks remain excluded from the discovery list; this filter does not replace operator checks.
- Asynchronous native ISO picker. Selection initially checks extension and readable, nonempty regular-file status; content validation happens during preparation.
- Exact-path confirmation, phase progress, cancellation, SHA-256 verification results, and complete error text available through `COPY ERROR DETAILS` where exposed. Error previews may be truncated.
- FAT32 labels: at most 11 ASCII characters, no control characters, forbidden FAT punctuation, or leading/trailing spaces. Internal spaces are permitted. Labels are uppercased; empty labels are allowed.
- Auto or custom 512 B through 32 KiB power-of-two clusters. The cluster must be at least the logical sector size and produce valid FAT32 cluster counts. The 34 MiB device guard is not a promise that every cluster/sector combination fits. A 4096-byte logical-sector test should use at least 512 MiB and valid geometry; small 4Kn volumes can be refused.

The GUI uses native iced rendering (wgpu/tiny-skia), not a browser. Rust 1.92 is required. UI dependencies are pinned to `iced = "=0.14.0"` and [`shadcn-iced = "=0.3.0"`](https://shadcn-iced.r4ai.dev/docs/installation). Buttons, selects, radio groups, checkboxes, inputs, and progress controls use the actual linked r4ai component library.

Text fonts come from Core Text on macOS, Windows `NONCLIENTMETRICS`, or fontconfig on Linux. No application text font is embedded; Geist/Inter features are disabled and only the component library's Lucide icon font is loaded. Colors follow OS light/dark preference at startup and while running, with light fallback and no in-app theme setting.

## Workspace

| Package | Path | Responsibility |
| --- | --- | --- |
| `rustfus-core` | `crates/rustfus-core` | Discovery, models, native disk guards, layouts, FAT32, bounded ISO parsing/staging, native WIM splitting, and verification |
| `rustfus-etc` | `crates/rustfus-etc` | iced desktop application, native ISO picker, confirmation, worker progress, and cancellation; provides `rustfus` |

Native OS calls use Rust platform bindings. There is no Rust-to-C++ bridge or runtime `wimlib`, DISM, or shell-helper dependency. OS/graphics libraries may themselves be implemented in C. External `wimlib` and DISM are optional independent manual validation tools, not runtime execution paths.

## Build and test locally

```text
cargo run --locked
cargo run --locked -p rustfus-etc --bin rustfus
```

Read-only device discovery, including fixed USB disks:

```text
cargo run --locked -p rustfus-core --example list_devices
```

Workspace checks and release build:

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

Capture the current running interface:

```text
cargo run -p rustfus-etc --example screenshot
```

The current desktop window is 620 x 640 logical units. OS scaling and window decoration can change the screenshot's pixel dimensions. `artifacts/rustfus-iced-preview.png` was captured from the compact current interface with the real UC300 USB detected; the screenshot workflow does not write media. Earlier audits describe earlier UIs; the current post-build review is `anti-slop/audit-003-2026-10-27.md` and is not hardware-write evidence.

### Linux dependencies

Ubuntu native GUI builds install:

```text
libdbus-1-dev libegl1-mesa-dev libfontconfig1-dev libfreetype6-dev
libgbm-dev libgl1-mesa-dev libudev-dev libwayland-dev libxkbcommon-dev
pkg-config xorg-dev
```

Runtime needs appropriate graphics drivers and libraries, including libxkbcommon, Wayland/EGL where used, and a Vulkan loader such as `libvulkan1` for the Vulkan backend. Software Mesa (for example, `mesa-vulkan-drivers` with lavapipe) can serve hosts without a usable GPU. Package names vary by distribution. A successful build does not prove a display or graphics backend is available. Core-only Linux checks do not need GUI development packages; discovery reads sysfs directly.

## CI and validation status

The intended native host matrix is ARM64 and x86_64 on all three operating systems. A 32-bit Windows **ISO** is different from a 32-bit **host build**, which is not supported.

| Host | x86_64 CI runner | ARM64 CI runner | Raw media/firmware validation |
| --- | --- | --- | --- |
| Linux | `ubuntu-24.04` | `ubuntu-24.04-arm` | Pending |
| macOS | `macos-15-intel` | `macos-15` | Pending |
| Windows | `windows-2025` | `windows-11-arm` | Pending |

`.github/workflows/ci.yml` defines six native jobs running formatting, all-target workspace tests, Clippy with warnings denied, and the release build above. Each uploads a raw `rustfus`/`rustfus.exe` binary, not an installer or self-contained bundle. Native runtime libraries can still be required. A separate Ubuntu quality job checks core formatting, compilation, and tests without GUI dependencies.

Current validation: 76 core tests and 13 desktop tests pass on macOS ARM64 (89 unique tests; the screenshot example repeats desktop tests). Workspace Clippy passes with warnings denied. Windows ARM64/x86_64 workspace checks and Linux x86_64 core checks pass. Evidence includes sparse-image layout/FAT32/copy/verification tests, an end-to-end synthetic Windows ISO fixture, and source WIM fixtures independently checked with external `wimlib`. These are not evidence of physical Windows Setup or Secure Boot compatibility; the full native CI matrix remains unrun locally.

**Not performed by the agent:** raw USB formatting, full-capacity physical media testing, physical UEFI boot, Secure Boot acceptance, or Windows installation. Native Windows/Linux runtime checks, platform theme/keyboard/scaling checks, and all physical-media rows remain pending unless a human has recorded them separately.

Use [docs/MANUAL_TESTING.md](docs/MANUAL_TESTING.md) for the complete checklist, per-host evidence template, negative preflight cases, native access restrictions, format matrix, cancellation/recovery, WIM independence checks, file hashes, and safe UEFI Setup smoke tests. Stop at Setup's initial language UI and abort; do not install onto an internal disk as part of validation.

## License

MIT OR Apache-2.0
