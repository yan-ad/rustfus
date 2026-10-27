# RustFus manual media checklist

This checklist covers destructive native media writes and the compact desktop UI. It is a manual test plan, not a claim that every item has passed. Leave every box unchecked until a human records evidence.

RustFus writes existing USB media. Use only a spare device whose contents may be destroyed. The agent has not performed raw hardware tests. Current automated evidence is limited to sparse image tests, workspace checks, Windows cross-compilation, and independent external checks of real source WIM fixtures with `wimlib`. `wimlib` is not a RustFus runtime dependency.

## Scope and non-goals

### Supported functional scope

- Native Rust disk access on Linux, macOS, and Windows.
- FAT32 only. NTFS and exFAT are not format targets.
- One aligned FAT32 partition with either MBR or GPT.
- Logical sectors of 512 or 4096 bytes, subject to FAT32 geometry limits.
- Quick format only. This is not a secure erase.
- Non-bootable FAT32 media.
- Windows installer media from a standard, trusted ISO using bounded ISO9660/Joliet or UDF parsing.
- Windows UEFI files copied from the source image, including supported x86, x86_64, and ARM64 EFI boot files when present. The copied files are source Microsoft-signed files. RustFus does not install a chainloader.
- FAT32 file sizes below 4 GiB. An oversized `install.wim` may be split into standard v1.13 `.swm` parts when the source is eligible.
- SHA-256 hashing while copying and reopening the FAT32 volume for round-trip verification.
- Cancellation, exclusive access, identity and capacity revalidation, durable flush, and native cleanup on the supported operating systems.

### Explicitly unsupported or intentionally refused

- DOS, FreeDOS, BIOS-only, Legacy BIOS, and old-BIOS media. These choices are not implemented and must not be represented as supported in the UI.
- NTFS and exFAT formatting.
- Secure erase or a promise that every write is recoverable after power loss.
- A universal root or administrator safety guarantee.
- Automatic privilege elevation. Linux needs root or equivalent raw-device permission, macOS needs permission to open the raw disk, and Windows needs Administrator rights. RustFus must not auto-elevate and must not run privilege scripts.
- Automatic Linux or macOS unmounting. Mounted media is refused. On Linux and macOS, unmount the volumes manually, but do not eject the disk.
- macOS APFS, HFS, CoreStorage, and unknown disk types. These are refused intentionally.
- Windows disks containing the running Windows installation, an EFI System Partition, recovery, service, platform-required, dynamic, or other unrelated protected volume. A Windows ESP or GPT boot Windows media may be conservatively rejected even if it is not the current system disk.
- Windows ISO content outside the bounded standard parser. Fragmented, sparse, embedded, multi-extent, continuation, or otherwise unsupported UDF and ISO allocations are refused.
- Solid, pipable, delta, already split, incomplete, unknown-flag, and nonstandard WIM inputs.
- An `install.esd` over 4 GiB. It is rejected before erase.
- An oversized WIM resource that cannot fit as one indivisible encoded resource in a FAT32 `.swm` part. It is rejected before erase.
- Hardware Secure Boot acceptance as a tested guarantee. The source EFI files are copied, but no hardware Secure Boot test has been performed by the agent.
- A guarantee that Windows Setup will select the intended target. The operator must not continue a destructive OS installation onto an internal disk.

## Safety rules for every destructive run

- [ ] Use a spare USB disk. Confirm its contents are backed up or disposable.
- [ ] Unplug every other removable or external disk that is not the test target.
- [ ] Record the target's display name, capacity, path or drive identity, serial if shown, removable/fixed status, and logical sector size before starting.
- [ ] Verify the target capacity twice: once in RustFus discovery and once in the operating system's disk utility or device information. The values must identify the same physical disk.
- [ ] Record the device path or physical drive identifier exactly. Never substitute a partition path, alias, mounted volume, or a different drive number.
- [ ] Put a unique marker on the test target before the run, such as a file containing `RUSTFUS-MANUAL-MARKER-DO-NOT-TRUST`. The marker is evidence for preflight tests only. Do not use a valuable disk.
- [ ] Confirm that the marker is still present before each test intended to fail before modification.
- [ ] Reconnect the target and record the path, serial, capacity, and drive ID again. If a drive ID is reused after replugging, select the newly discovered device and repeat the identity and capacity checks.
- [ ] Test absent, duplicate, and all-zero serial numbers where available. Expected: no invented unique serial or serial-only safety claim. Repeat physical path/capacity checks on every replug; indistinguishable replacement devices are not universally detectable.
- [ ] Verify that the app shows the selected whole USB disk, not one option per partition.
- [ ] Read the destructive confirmation text. Type the exact selected device path, including case and separators.
- [ ] Test a mismatched confirmation and cancel the confirmation dialog. Expected: no erase, no write, marker remains, and settings remain available.
- [ ] Confirm that the final destructive action is never taken on an internal disk, the running Windows disk, or a disk with irreplaceable data.
- [ ] Treat a power loss, unplug, reset, or OS crash during partitioning, formatting, copying, verification, or flushing as a failed media state. Reformat the spare target before reuse.
- [ ] Do not assume native cleanup prevents an operating system from auto-remounting media after the write. Inspect the device again after completion.

## Test record

Copy this block for each host and physical target. Do not publish sensitive serial numbers or raw device identifiers in public logs.

| Field | Record |
| --- | --- |
| Tester and date | |
| Host OS and version | |
| Host architecture | |
| RustFus commit or build | |
| Rust/RustFus version | |
| Target display name | |
| Target capacity, check 1 | |
| Target capacity, check 2 | |
| Target logical sector size | |
| Target serial, private record only | |
| Target path or drive ID, private record only | |
| MBR or GPT | |
| FAT32 cluster selection | |
| Volume label | |
| ISO filename, source architecture, and size | |
| Source ISO SHA-256 | |
| Start time | |
| Completion time | |
| Result and exact error text | |
| Evidence path, kept private if needed | |

Result: `PASS / FAIL / BLOCKED`  Notes:

## Build and baseline checks

- [ ] On the host, run `cargo fmt --all -- --check`. Expected: success.
- [ ] Run `cargo test --workspace --all-targets`. Expected: success, including sparse MBR/GPT FAT32 round trips, cancellation and preflight preservation tests, parser bounds tests, and WIM splitter tests.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`. Expected: success.
- [ ] Run `cargo build -p rustfus-etc --bin rustfus --release --locked`. Expected: success for the host target.
- [ ] Run `cargo run --locked`. Expected: the native UI opens.
- [ ] Run `cargo run --locked -p rustfus-etc --bin rustfus`. Expected: the same application opens.
- [ ] Run `cargo run --locked -p rustfus-core --example list_devices`. Expected: real USB device discovery output only, with no mock devices.
- [ ] Run `cargo run -p rustfus-etc --example screenshot`. Expected: a real capture of the current compact UI, using the current 620 x 640 minimum window rather than the old larger layout.
- [ ] Record any build, runtime, or test failure verbatim. Do not convert a blocked hardware test into a pass.

## UI and accessibility checklist

### Window and layout

- [ ] Open the app at the default 620 x 640 window size. Expected: all primary controls fit without a form scroll region.
- [ ] Verify the minimum window remains 620 x 640. Expected: the compact fixed-size composition does not revert to the old large-title or large-window design.
- [ ] Verify large decorative titles are absent. Section headings are compact and functional.
- [ ] Test 100%, 150%, and 200% OS display scaling where available. Expected: controls remain reachable, text remains legible, and no important control is clipped.
- [ ] Test light and dark OS themes. Expected: the UI follows the OS theme, with readable controls and no in-app theme setting.
- [ ] Change the OS theme while the app is open. Expected: colors update without losing device or ISO selection.
- [ ] Test keyboard navigation with Tab and Shift+Tab. Expected: focus moves through controls in a sensible visual order.
- [ ] Activate buttons, selects, radios, checkboxes, and the confirmation input with keyboard controls. Expected: Enter or Space performs the same action as pointer activation.
- [ ] Verify focused controls have a visible focus indicator.
- [ ] Connect or disconnect another USB disk while the app is open. Expected: discovery continues and the selected target does not silently switch or freeze.
- [ ] Close the window while a write is running. Expected: the request becomes cancellation, the window stays open, controls remain locked until cleanup finishes, and the final result reports success or incomplete media.
- [ ] Close the window while the ISO picker is active. Expected: the picker state is handled without an accidental write.

### Discovery and settings

- [ ] Start with no eligible USB disk. Expected: an empty waiting state, not a fake device.
- [ ] Connect a USB flash disk. Expected: it appears automatically without a refresh button.
- [ ] Unplug it while the app is open. Expected: it disappears on a later poll.
- [ ] Connect a multipartition USB disk. Expected: it appears once as a whole disk.
- [ ] Confirm internal disks remain excluded.
- [ ] Enable `Include fixed USB disks`. Expected: an eligible fixed USB SSD or hard disk may appear, while internal disks remain excluded.
- [ ] Select `Non Bootable`. Expected: no ISO is required, and the target selection does not offer Legacy BIOS.
- [ ] Select `Windows ISO`. Expected: the ISO picker is available and the target is UEFI only.
- [ ] Verify DOS and FreeDOS are absent or disabled and cannot be selected.
- [ ] Verify the filesystem control exposes FAT32 only. Expected: no NTFS or exFAT choice is presented.
- [ ] Verify partition choices are MBR and GPT only.
- [ ] Verify cluster choices are Auto, 512 B, 1 KiB, 2 KiB, 4 KiB, 8 KiB, 16 KiB, and 32 KiB.
- [ ] Verify the app describes Windows ISO media as UEFI only and does not imply BIOS or universal Windows support.

### ISO picker and errors

- [ ] Choose a readable, nonempty regular `.iso`. Expected: the path and size are shown, without claiming that ISO contents have already been validated.
- [ ] Cancel the picker. Expected: the previous ISO selection remains unchanged.
- [ ] Choose a non-ISO extension, unreadable path, empty file, directory, or other non-regular file. Expected: selection fails before confirmation and the error can be copied.
- [ ] Trigger a long error. Expected: the UI may show a shortened preview, but `COPY ERROR DETAILS` places the complete error in the clipboard.
- [ ] Verify an error does not enable the destructive action.

## Preflight and no-modification tests

For every case in this section, verify the target marker and a small raw metadata sample remain unchanged. A failure before opening or staging must not erase the disk.

- [ ] Select no device. Expected: write is disabled and no disk is opened.
- [ ] Select a read-only target. Expected: the write action is disabled or fails before opening, with a clear read-only error.
- [ ] Use a mounted target on Linux. Expected: RustFus refuses it. Manually unmount the volume, do not eject the disk, and retry.
- [ ] Use a mounted target on macOS. Expected: RustFus refuses it with an instruction to unmount volumes, not eject the disk.
- [ ] Use a target that another process keeps busy. Expected: the native guard refuses it and reports the application or access condition where the OS provides one.
- [ ] Use a path that is a partition, alias, mounted volume, or noncanonical device path. Expected: the native guard refuses it.
- [ ] Deny the required Linux root, macOS raw-disk, or Windows Administrator permission. Expected: the operation fails with a permission error. No auto-elevation or script is launched.
- [ ] Type a confirmation string that differs by one character, case, separator, or whitespace. Expected: erase remains unavailable.
- [ ] Cancel the confirmation dialog. Expected: the marker and metadata remain unchanged.
- [ ] Remove the target after discovery and before confirmation. Expected: the operation refuses the missing disk.
- [ ] Replug the target so the old path or drive ID is reused for a different disk. Expected: identity, serial where available, capacity, removable state, or discovery ID mismatch causes refusal. Select the newly discovered disk explicitly.
- [ ] Use a target whose capacity changes or whose logical sector size changes between discovery and open. Expected: refusal before the first destructive write.
- [ ] Use a capacity below the aligned FAT32 minimum. Expected: rejection before opening and before erase.
- [ ] Use a capacity that cannot fit the requested staged files. Expected: the source is fully staged and fit-checked first, then rejection occurs before partition-table modification.
- [ ] Use an oversized `install.esd` above 4 GiB. Expected: rejection before erase.
- [ ] Use an oversized solid, pipable, delta, already split, incomplete, unknown-flag, unsupported-codec, malformed, or nonstandard WIM requiring the splitter. Expected: rejection before erase.
- [ ] Use an oversized indivisible WIM encoded resource that cannot fit a split part. Expected: rejection before erase.
- [ ] Use a malformed, fragmented, sparse, embedded, multi-extent, continuation, or out-of-bounds ISO9660/UDF fixture. Expected: bounded parser rejection before erase.
- [ ] Use an ISO without `setup.exe`, `sources/boot.wim`, a supported EFI boot file, or an install image. Expected: rejection before erase.
- [ ] Cancel while source parsing or WIM splitting is staging. Expected: no target partition-table write, no target marker loss, and temporary generated parts are cleaned up.
- [ ] Make the source ISO change size or modification time while staging. Expected: staging fails before opening the target.
- [ ] Remove source read permission or make a source file inaccessible. Expected: a detailed source error before target modification.
- [ ] Verify the error includes the operation, affected path or phase, and cleanup status. Expected: no generic success message hides the failure.

## Native platform access checks

### Linux

- [ ] Run with the required root or raw-device permission. Expected: the operation can open only a canonical whole `/dev` block device.
- [ ] Test a mounted partition, active swap, device-mapper holder, or MD holder. Expected: refusal.
- [ ] Add a mount after the initial discovery and before the write. Expected: the second mount snapshot catches it and refuses access.
- [ ] Test a non-USB direct-access disk. Expected: refusal.
- [ ] Test a read-only disk. Expected: refusal.
- [ ] Test a 512-byte logical-sector target. Expected: geometry is accepted when the FAT32 layout is valid.
- [ ] Test a 4096-byte logical-sector target. Expected: geometry is accepted when the FAT32 layout is valid.
- [ ] Verify the device is synchronized and the partition table reread is attempted while the exclusive handle is still held.
- [ ] Verify a failed synchronization or partition reread reports incomplete media and does not claim success.

### macOS

- [ ] Run with permission to open the raw whole-disk node. Expected: no automatic elevation.
- [ ] Use a canonical whole-disk path such as `/dev/diskN`, not a slice or arbitrary file. Expected: slices and aliases are refused.
- [ ] Test mounted volumes. Expected: Disk Arbitration and the mount snapshot refuse access. Unmount volumes manually, but do not eject the disk.
- [ ] Test APFS, HFS, CoreStorage, Apple backing stores, and unknown partition types. Expected: intentional refusal.
- [ ] Test a disk identity change after claim or replug. Expected: registry identity mismatch refuses the write.
- [ ] Test 512-byte and 4096-byte logical sectors. Expected: supported geometry proceeds to format preflight when the partition is valid.
- [ ] Verify the Disk Arbitration claim and mount veto remain active through raw writing, flush, and cleanup.
- [ ] Verify a cache flush failure reports incomplete media and preserves the full error.

### Windows

- [ ] Run as Administrator. Expected: no auto-elevation prompt or helper script is launched by RustFus.
- [ ] Verify the target path is a canonical whole `\\.\PhysicalDriveN`, not a partition or arbitrary file.
- [ ] Test a target with an existing EFI System Partition, Windows recovery or service partition, platform-required GPT attribute, dynamic-disk partition, or another protected partition. Expected: conservative refusal.
- [ ] Test the disk containing the running Windows installation. Expected: refusal.
- [ ] Test a multi-disk or spanned volume. Expected: refusal.
- [ ] Verify every target volume is locked and dismounted by native Windows calls only, with no sharing fallback.
- [ ] Introduce a new target volume during acquisition. Expected: the guard detects the changed volume set and refuses the operation.
- [ ] Test serial identity, removable status, capacity, and logical sector changes between discovery and open. Expected: refusal.
- [ ] Test 512-byte and 4096-byte logical sectors. Expected: supported geometry proceeds to format preflight when valid.
- [ ] Verify disk properties are refreshed while volume locks remain held, and handles are released only after synchronization and cleanup.

## FAT32 formatting matrix

For each row, use a fresh spare target or reformat the target after any prior destructive or incomplete run. Record the actual capacity and sector size.

For 4Kn (4096-byte logical-sector) positive cases use at least 512 MiB and verify the actual FAT32 geometry. The 34 MiB device guard is not a sufficient positive geometry guarantee. A 512e device has 512-byte logical sectors even if its physical sectors are 4096 bytes; record both sizes when available.

| Case | Scheme | Sector size | Cluster | Label | Expected result | Result |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | MBR | 512 | Auto | `RUSTFUS` | Quick format, one FAT32 partition | [ ] |
| 2 | GPT | 512 | Auto | `RUSTFUS` | Quick format, one FAT32 partition | [ ] |
| 3 | MBR | 512 | 512 B | `USB512` | Format succeeds if FAT32 cluster bounds fit | [ ] |
| 4 | GPT | 512 | 32 KiB | `USB32K` | Format succeeds if FAT32 cluster bounds fit | [ ] |
| 5 | MBR | 4096 | Auto | `SECTOR4K` | Format succeeds on a real 4 KiB logical-sector target | [ ] |
| 6 | GPT | 4096 | 4 KiB or Auto | `GPT4K` | Format succeeds on a real 4 KiB logical-sector target | [ ] |
| 7 | MBR | 4096 | below sector size | `BAD` | Rejected before erase | [ ] |
| 8 | GPT | 4096 | 32 KiB | `BOUNDARY` | Succeeds only when FAT32 geometry is valid | [ ] |

- [ ] Inspect MBR output. Expected: one FAT32 LBA partition, aligned start, valid 0x55AA signature, and no BIOS boot loader.
- [ ] Inspect GPT output. Expected: protective MBR, primary and backup GPT headers, matching entry arrays and CRCs, and one FAT32 partition.
- [ ] For GPT Windows ISO mode, inspect the partition type. Expected: EFI System Partition type is used as requested by the native layout writer.
- [ ] For GPT non-bootable mode, inspect the partition type. Expected: Microsoft Basic Data type is used.
- [ ] Verify the partition starts at the planned 1 MiB alignment for both sector sizes.
- [ ] Verify the final partition does not exceed disk capacity.
- [ ] Verify quick format does not claim to erase all old data. A raw inspection may find old data outside newly written metadata.
- [ ] Mount or inspect the result with the host disk utility. Expected: FAT32 is recognized and the label is uppercase and space-padded as appropriate.
- [ ] Create a small file, unmount, remount, and read it back. Expected: exact contents survive.
- [ ] Attempt to create a file larger than 4 GiB. Expected: FAT32 or the copying preflight rejects it; no claim of support is made.
- [ ] Check reported full usable capacity and filesystem free-space statistics against the expected formatted geometry.
- [ ] On the spare target only, fill usable space with deterministic test files smaller than 4 GiB each, stopping on normal full-volume reporting. Unmount/remount and hash all test files. Expected: data spans usable capacity without corruption, silent wraparound, or false capacity. Remove files afterward. RustFus does not perform full-capacity certification itself.
- [ ] Verify a non-bootable target has an empty root before the test file is added.
- [ ] Confirm the target's partition table and FAT metadata are readable after unplugging and reconnecting only after the operation has completed.

### Label and cluster validation

- [ ] Use an empty label if the UI permits it. Expected: FAT32 label validation follows the current implementation and does not invent a label.
- [ ] Use labels of 1 and 11 ASCII characters. Expected: accepted and uppercased.
- [ ] Use a 12-character label. Expected: rejected before erase.
- [ ] Use leading or trailing whitespace. Expected: rejected before erase.
- [ ] Use internal spaces in an otherwise valid label. Expected: accepted if FAT label rules permit it.
- [ ] Use non-ASCII text. Expected: rejected before erase.
- [ ] Use control characters, DEL, or FAT punctuation such as `" * + , / : ; < = > ? [ \\ ] |`. Expected: rejected before erase.
- [ ] Use each cluster choice from Auto through 32 KiB on a geometry where it is valid. Expected: the selected cluster size is visible in filesystem metadata.
- [ ] Use a cluster size that is zero, not a power of two, below the logical sector size, or above 32 KiB through a crafted request or validation fixture. Expected: rejected before any device access.
- [ ] Use a volume too small or too large for FAT32's supported geometry. Expected: rejected before erase.

## Cancellation and failure recovery

- [ ] Cancel during source staging. Expected: target marker and raw metadata remain unchanged.
- [ ] Cancel during device opening or native claim. Expected: no raw write starts, and the target remains unchanged.
- [ ] Cancel during partitioning. Expected: the operation reports incomplete media; do not reuse the target until it is reformatted.
- [ ] Cancel during FAT32 formatting. Expected: the operation reports incomplete media; reformat before reuse.
- [ ] Cancel during file copying. Expected: the operation reports incomplete media; reformat before reuse.
- [ ] Cancel during SHA-256 verification. Expected: the operation reports incomplete media or failed verification; reformat before reuse.
- [ ] Cancel during flushing. Expected: native cleanup and synchronization still run; the final state is not reported as a clean success unless all required cleanup succeeds.
- [ ] Trigger a simulated source read failure, target write failure, sync failure, or unmount/cleanup failure where the test harness permits. Expected: the final error includes the primary failure and cleanup failure when both occur.
- [ ] While running, verify settings, target selection, ISO picker, label, scheme, cluster, and filesystem controls are locked.
- [ ] Press Cancel repeatedly. Expected: one cancellation request, no duplicate worker, and the window remains responsive.
- [ ] After a failed or cancelled run, do not trust a retained selection or ordinary ready state as proof of usable media. Expected: the failed result remains explicit until settings are edited. Reformat the target before reuse.
- [ ] Optional isolated-lab test only: unplug the spare target during copying or verification. Expected: detailed I/O failure or incomplete media, no success claim, and no redirection to another disk reusing its identifier. OS auto-remount after reconnection is not universally prevented.
- [ ] Optional isolated-lab test only: interrupt power to the spare target during a write. Expected: no rollback guarantee; after restart, treat the target as incomplete and reformat it. Record unobservable cleanup as blocked, not passed. Never interrupt power to a production host.

## Windows ISO content and UEFI checklist

Use official Microsoft Windows 10 and Windows 11 ISO files where legally available. Record the official source and SHA-256 before selecting the ISO. Boot architecture does not need to match the host architecture.

### Source and parser coverage

- [ ] Test a trusted Windows 10 x64 ISO with a recorded official SHA-256.
- [ ] Test a trusted Windows 11 x64 ISO with a recorded official SHA-256.
- [ ] Test a trusted Windows ISO containing ARM64 UEFI files, if an official fixture is available.
- [ ] Test a trusted Windows ISO containing x86 UEFI files, if an official fixture is available.
- [ ] Test a standard ISO9660/Joliet image. Expected: bounded parsing and staging succeed when all required files are valid.
- [ ] Test a standard UDF image. Expected: bounded parsing and staging succeed when all required files are valid.
- [ ] Test a UDF image with fragmented or unsupported allocation. Expected: explicit rejection, not fallback to a truncated bridge tree.
- [ ] Test an ISO with an extent beyond the image length. Expected: rejection before erase.
- [ ] Test an ISO with excessive directory depth, entry count, or directory bytes. Expected: bounded rejection before erase.
- [ ] Test duplicate case-insensitive paths and Windows-illegal names. Expected: rejection before erase.
- [ ] Test an image without `setup.exe`, `sources/boot.wim`, a supported `EFI/BOOT/BOOTX64.EFI`, `BOOTAA64.EFI`, or `BOOTIA32.EFI`, or any install image. Expected: rejection before erase.
- [ ] Test a source containing an unsupported `install.esd` above 4 GiB. Expected: rejection before erase.
- [ ] Test a valid `install.esd` below 4 GiB. Expected: it is staged without claiming ESD conversion or decompression.
- [ ] Test an `install.wim` below 4 GiB. Expected: it remains `sources/install.wim` and is copied.
- [ ] Test an `install.wim` above 4 GiB. Expected: it is split before target opening into `install.swm`, `install2.swm`, and sequential parts as needed. The original oversized `install.wim` is not copied to FAT32.
- [ ] Test an ISO that already contains `install.swm`, `install2.swm`, and consecutive parts. Expected: parts are validated and copied; missing, zero-length, duplicate, or out-of-sequence parts are rejected.
- [ ] Test an ISO containing both an oversized `install.wim` and existing split parts. Expected: rejection before erase.
- [ ] Test standard v1.13 WIMs using no compression, XPRESS, and LZX where the fixture is structurally eligible. Expected: the splitter preserves encoded resources and metadata structure without runtime decompression.
- [ ] Test oversized solid, pipable, delta, incomplete, already split, unknown-flag, malformed, and nonstandard v1.13 WIM fixtures requiring splitting. Expected: rejection before erase.
- [ ] Test an indivisible encoded resource larger than the configured FAT32 `.swm` part budget. Expected: rejection before erase.
- [ ] Test source mutation while WIM inspection or splitting runs. Expected: generated parts are removed and the target is untouched.
- [ ] Verify source WIM SHA-1/resource hashes are preserved structurally by the splitter, but do not describe this as decompression or image-content authentication.

### External WIM fixture verification

- [ ] Keep a copy of each source fixture and its SHA-256 outside the build tree.
- [ ] Run an independent `wimlib` check or extraction against the original source WIM fixture where supported. Expected: the original fixture is independently readable.
- [ ] Run the independent check against the generated `.swm` set using `wimlib` where supported. Expected: the split set is independently readable and lists the expected images.
- [ ] Compare generated part names, ordering, sizes, and source-to-output hashes. Expected: no original oversized `install.wim` appears on the FAT32 target; all generated parts are sequential.
- [ ] Record the exact `wimlib` version and command in private test notes. Do not add `wimlib` as a RustFus runtime dependency.
- [ ] If `wimlib` cannot read a fixture because it is outside the supported standard set, record `BLOCKED` or `FAIL`, not a pass.
- [ ] Optional Windows independent check: use DISM `/Get-WimInfo` to inspect each image index in `sources/install.swm`. Where supported, validate/apply the complete set using `/SWMFile` only to a disposable lab directory or image, never an internal installation target. Record DISM version, exact command, and result. Listing alone is not full resource verification. DISM is a manual tool, not a runtime bridge.

### UEFI and Windows Setup test

- [ ] Verify the selected Windows ISO target is UEFI only. Expected: BIOS or Legacy is unavailable and an attempted Legacy request is rejected.
- [ ] Verify the copied EFI files have the expected source SHA-256 before and after copying where practical.
- [ ] Verify RustFus does not alter source EFI binaries or install a chainloader.
- [ ] On spare hardware, boot the created media in UEFI mode before selecting an install target. Expected: Windows Setup reaches its language or initial setup UI.
- [ ] Test x64 media on x64 UEFI hardware, ARM64 media on ARM64 UEFI hardware, and x86 media on x86 UEFI hardware when available. Host architecture does not need to match the ISO architecture.
- [ ] Test Secure Boot only as an optional hardware observation. Record whether firmware accepts the source-signed files, but do not claim Secure Boot support from source inspection alone.
- [ ] Do not click through to destructive installation on an internal disk. Stop at the language or initial setup screen and abort.
- [ ] Record that reaching Setup does not guarantee that Windows Setup will select the intended target. The operator must choose the target carefully and must not continue installation on an internal disk.
- [ ] If firmware cannot boot the media, record firmware mode, Secure Boot state, ISO architecture, target scheme, and exact visible error. Do not infer a parser or copy failure without evidence.

### End-to-end Windows media run

- [ ] Record the official source ISO SHA-256.
- [ ] Select a spare target and verify its capacity twice.
- [ ] Select GPT or MBR as the planned test case and UEFI target.
- [ ] Confirm the exact device path and start the confirmation.
- [ ] Type the exact path and begin the write.
- [ ] Observe `Preparing`, `Opening`, `Partitioning`, `Formatting`, `Copying`, `Verifying`, and `Flushing` where the operation is long enough to expose them.
- [ ] Verify progress remains responsive and controls stay locked.
- [ ] Verify the result reports the copied file count, bytes, detected UEFI architecture, and successful SHA-256 round-trip verification.
- [ ] Reopen the target with the operating system and inspect the FAT32 filesystem, label, partition table, `EFI/BOOT`, `setup.exe`, `sources/boot.wim`, and install image or sequential `.swm` files.
- [ ] Compare source and target file hashes for selected files, excluding the original oversized `install.wim` when it was split.
- [ ] Unplug and reconnect only after flushing is complete. Expected: the target is rediscovered with its new filesystem and no stale result is shown.

## SHA-256 round-trip verification

- [ ] In the sparse-image operation harness, use staged small and empty files. Expected: both files are copied and reopened. The Non Bootable UI mode creates an empty volume and does not offer arbitrary file copying.
- [ ] Use a Windows fixture with nested directories and files at different sizes. Expected: every staged file is reopened, size checked, and SHA-256 checked.
- [ ] Modify one target file after copying but before verification where a controlled test harness permits it. Expected: same-size corruption is reported as SHA-256 verification failure.
- [ ] Modify a source file during staging. Expected: the source mutation is reported and no target write starts.
- [ ] Confirm verification occurs only after raw synchronization and filesystem remount/reopen.
- [ ] Confirm a verification failure never produces a success result.
- [ ] Confirm an empty non-bootable filesystem contains no unexpected root entries before user-created files are added.

## Six-architecture CI and cross-build record

This table mirrors the six native CI jobs. CI proves compilation and automated checks for the job, not raw hardware behavior.

| CI target | CI job | Format/tests | Release build | Native hardware | Notes |
| --- | --- | --- | --- | --- | --- |
| Linux x86_64 | [ ] | [ ] | [ ] | [ ] | |
| Linux ARM64 | [ ] | [ ] | [ ] | [ ] | |
| macOS x86_64 | [ ] | [ ] | [ ] | [ ] | |
| macOS ARM64 | [ ] | [ ] | [ ] | [ ] | |
| Windows x86_64 | [ ] | [ ] | [ ] | [ ] | |
| Windows ARM64 | [ ] | [ ] | [ ] | [ ] | |

- [ ] Confirm Windows x86_64, Windows ARM64, Linux x86_64, Linux ARM64, macOS x86_64, and macOS ARM64 release builds complete where the CI environment is available.
- [ ] Confirm `cargo test --workspace --all-targets` runs for the native jobs.
- [ ] Confirm Clippy warnings are denied for the native jobs.
- [ ] Confirm artifacts are raw binaries, not installers or self-contained bundles.
- [ ] Record that CI does not prove display, graphics-driver, raw disk, UEFI firmware, Secure Boot, or physical media behavior.

## Final evidence review

- [ ] Every manual row has `PASS`, `FAIL`, or `BLOCKED` and a note.
- [ ] Every destructive result names the spare target and private target identity record.
- [ ] No public report contains a real serial number, full device path, or other sensitive hardware identifier.
- [ ] No test claims universal root safety, universal Windows installation support, BIOS support, Secure Boot acceptance, or secure erase.
- [ ] Failures before erase have evidence that the marker or raw metadata was preserved.
- [ ] Failures after partial write have evidence that the target was reformatted before reuse.
- [ ] Native cleanup, sync, partition refresh, and error details are recorded.
- [ ] The final validation totals are filled from these records, not inferred from compilation or sparse image tests.
