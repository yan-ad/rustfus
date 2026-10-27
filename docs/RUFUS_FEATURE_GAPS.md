# RustFus feature gaps against Rufus

**Audit snapshot:** 2026-10-27
**Rufus baseline:** 4.15, build 2396, tag `v4.15`, released 2026-06-30. The official site and the authenticated `pbatard/rufus` release endpoint both identify 4.15 as the current release at the time of this audit.
**RustFus baseline:** repository state audited on 2026-10-27.
**Scope:** user-facing Rufus feature families and notable advanced/hidden capabilities, compared with the current RustFus README, manual checklist, core model, media implementation, and UI.
**Change status:** this audit changes only this document. It makes no implementation promise or schedule.

## Executive result

RustFus currently covers a narrow, deliberately safer subset: native cross-platform device discovery and guarded destructive access; one aligned MBR or GPT FAT32 volume; quick formatting; a non-bootable mode; and a bounded Windows ISO-to-FAT32 UEFI file-copy path with native splitting of eligible oversized `install.wim` files and SHA-256 round-trip verification.

The largest parity gaps are:

1. **P0 safety/correctness:** no Rufus-quality raw image/DD path, no broad source/image validation, no bootloader installation or runtime UEFI-media validation, and no physical-media, firmware, or Secure Boot evidence. These are not merely missing toggles: implementing them safely requires new parsers, writers, platform behavior, and test evidence.
2. **P1 core parity:** only FAT32 is available; NTFS, exFAT, FAT16, UDF, ReFS, ext2/ext3/ext4, BIOS/Legacy boot, DOS, Linux/other ISO families, Windows To Go, compressed/VHD/VHDX/FFU images, and Rufus' bootloader integrations are absent. FAT32 above 32 GiB is possible, but Rufus' Large FAT32 selection/default policies are not reproduced.
3. **P1 Windows parity:** no Windows User Experience/unattended options, TPM/Secure Boot/RAM bypass, local-account or regional setup automation, BitLocker choice, QoL changes, CA 2023 bootloader selection, revoked-bootloader/DBX checks, or silent-install safeguards.
4. **P2 product parity:** no Rufus download/Fido workflow, localization system, portable settings mode, update check, persistent log, saved log dialog, advanced/expert modes, extended label/icon files, configurable retry/zeroing/bad-block passes, or broad image-source UX.

“Not implemented” below means no corresponding RustFus runtime path or user-facing control was found. “Validation only” means RustFus has a check or test fixture, but not the Rufus operation or a runtime guarantee. “Partial” means a narrower RustFus capability exists.

## Status vocabulary

| Status | Meaning |
| --- | --- |
| **Implemented** | RustFus has the capability in its current runtime/UI. This is not a claim that physical hardware validation has passed. |
| **Partial** | A smaller or more restricted version exists. |
| **NotImplemented** | No runtime implementation or user-facing control was found. |
| **SafetyCompatibilityGap** | The feature is absent and its absence affects bootability, source compatibility, integrity, or destructive-write safety. |
| **ValidationOnly** | Code/tests or documentation cover a check, but not end-to-end physical, firmware, or installation behavior. |
| **OutOfScope** | Rufus behavior is Windows-specific or intentionally outside RustFus' current cross-platform product boundary. |

## Feature matrix

### 1. Media modes, images, and operating systems

| Rufus family | RustFus status | Gap and evidence |
| --- | --- | --- |
| Non-bootable format | **Partial / Implemented** | RustFus creates one aligned FAT32 partition with MBR or GPT and quick-formats it. It does not offer Rufus' broad filesystem choices, erase passes, label/icon extras, or bad-block workflow. |
| ISO/file-copy mode | **Partial** | A bounded Windows ISO path exists. It is not a general ISO writer and rejects content outside the supported ISO9660/Joliet and physical-partition UDF subset. |
| ISOHybrid choice: ISO mode vs DD/raw mode | **SafetyCompatibilityGap** | Rufus detects ISOHybrid images and can ask for ISO or DD mode, or enforce DD mode. RustFus has no raw image/DD operation and cannot safely represent this choice. |
| Arbitrary raw image writing | **SafetyCompatibilityGap** | No general sector-for-sector image writer, image-size/target-size policy, progress model, or post-write image verification is exposed. |
| Save a physical disk/optical source as ISO | **NotImplemented / OutOfScope** | Rufus includes the advanced optical/source-to-ISO workflow. RustFus has no source-device imaging path. |
| Windows standard installer ISO | **Partial** | RustFus stages required Windows files, copies supported UEFI files, and can split eligible oversized WIMs. It does not provide Rufus' full Windows image, filesystem, boot, or WUE matrix. |
| Windows To Go | **NotImplemented / SafetyCompatibilityGap** | No applied Windows image, NTFS-only WTG layout, GPT MSR/ESP layout, fixed/removable policy, internal-disk protection in the generated Windows environment, or WTG-specific unattended options. |
| Linux installer ISO families | **NotImplemented / SafetyCompatibilityGap** | No Linux ISO staging or bootloader handling. This includes the official site's examples such as Ubuntu, Debian, Fedora, Arch, Linux Mint, Kali, Tails, openSUSE, Gentoo, Clonezilla, GParted, and related distributions. |
| DOS boot media | **NotImplemented** | MS-DOS and FreeDOS choices are intentionally absent and rejected by the retained model variants. Legacy DOS boot sectors and keyboard-layout selection are absent. |
| Other boot media | **NotImplemented** | No dedicated ReactOS, KolibriOS, UEFI Shell, Windows XP-era, WinPE, Hiren's, Super Grub2, or similar boot-media handling. Rufus has image detection and special cases for several of these. |
| Compressed images/archives | **NotImplemented** | No `.zip`, `.Z`, `.gz`, `.bz2`, `.xz`, `.lzma`, `.zst`, or Rufus VTSI compressed-image extraction path. |
| VHD/VHDX | **NotImplemented** | No VHD or compressed VHDX source handling, mounting/application, target-size checks, or extraction progress. |
| FFU | **NotImplemented / OutOfScope** | Rufus can use Full Flash Update images where the Windows host provides the required support. RustFus has no FFU path. |
| Downloadable Windows ISO/Fido | **NotImplemented / OutOfScope** | Rufus can invoke Fido to list/download supported Windows releases. RustFus only accepts a user-selected local `.iso`; it does not download source media. |
| Persistent Linux partition | **NotImplemented** | No persistence-size control, separate persistence partition, or distribution-specific persistence setup. |

### 2. Filesystems, layouts, and bootloaders

| Rufus capability | RustFus status | Gap and evidence |
| --- | --- | --- |
| FAT16 | **NotImplemented** | Rufus exposes FAT16 for applicable media and boot modes. RustFus exposes FAT32 only. |
| FAT32 | **Partial / Implemented** | RustFus supports FAT32, 512/4096-byte logical sectors, Auto through 32 KiB clusters, MBR/GPT, aligned partitioning, and quick format. It does not match all Rufus geometry/default/bootloader behavior. |
| Large FAT32 | **Partial** | RustFus does not impose the Windows formatter's 32 GiB cap and can format larger FAT32 volumes within supported geometry. It does not reproduce Rufus' separate Large FAT32 mode, forced-mode toggle, cluster defaults, or compatibility policy. |
| NTFS | **NotImplemented / SafetyCompatibilityGap** | No NTFS formatter, NTFS cluster policy, compression, Windows To Go target, or UEFI:NTFS support. |
| exFAT | **NotImplemented** | No exFAT formatter or UEFI:NTFS-compatible exFAT path. |
| UDF | **NotImplemented** | RustFus reads a bounded UDF subset as an ISO source, but does not create UDF media. Rufus can format UDF. |
| ReFS | **NotImplemented / OutOfScope** | Rufus conditionally exposes ReFS on supported Windows editions/media. RustFus has no ReFS writer and is cross-platform. |
| ext2/ext3/ext4 | **NotImplemented** | Rufus exposes ext2/ext3/ext4 in advanced bootloader paths. RustFus has no ext filesystem formatter. |
| MBR/GPT | **Partial** | RustFus writes one aligned partition with MBR or GPT and uses mode-specific partition types. Rufus also supports target-system-dependent layouts, SFD/other special cases, extra partitions, and bootloader-specific metadata. |
| BIOS/Legacy target | **NotImplemented / SafetyCompatibilityGap** | RustFus offers UEFI only for Windows ISO and has no BIOS loader. No active partition, BIOS boot code, Syslinux, GRUB, GRUB4DOS, ReactOS, or DOS boot-sector installation exists. |
| UEFI:NTFS | **NotImplemented / SafetyCompatibilityGap** | No bundled UEFI:NTFS bootloader, NTFS/exFAT data partition, Secure Boot warning, or firmware handoff. |
| Syslinux/ISOLINUX | **NotImplemented** | No embedded/downloaded Syslinux files, version detection, replacement of obsolete modules, or BIOS/UEFI integration. |
| GRUB2/GRUB4DOS | **NotImplemented** | No embedded/downloaded GRUB assets, version matching, MBR installation, or ISO-specific GRUB compatibility logic. |
| Rufus MBR / old BIOS fixes | **NotImplemented** | No Rufus MBR choice, extra partition, alignment workaround, BIOS drive ID policy, or old-BIOS fixes. |
| Partition flags/types | **Partial** | RustFus emits the intended basic FAT32/GPT structures, but does not expose Rufus' advanced partition-type, active/boot flag, extra partition, or bootloader-specific controls. |
| ISO → ESP special path | **NotImplemented** | Rufus can offer a small regular-EFI ISO as an ESP on GPT/FAT media. RustFus has no equivalent decision or layout. |
| Bootloader architecture coverage | **Partial / ValidationOnly** | RustFus recognizes/copies Windows x86, x64, and ARM64 EFI files when present. RISC-V and other Rufus UEFI:NTFS/bootloader paths are absent. Physical boot and firmware acceptance remain untested. |

### 3. Windows installation customization and compatibility

| Rufus capability | RustFus status | Gap and evidence |
| --- | --- | --- |
| Windows User Experience dialog | **NotImplemented** | RustFus has no second-stage Windows customization dialog or persisted WUE choices. |
| TPM 2.0, Secure Boot, and 4 GB RAM bypass | **NotImplemented / SafetyCompatibilityGap** | No `autounattend.xml` generation for Windows 11 setup requirement bypasses. |
| Online Microsoft-account bypass | **NotImplemented** | No offline-account setup option. |
| Local account and username | **NotImplemented** | No local-account generation, username input/validation, or first-logon password behavior. |
| Regional duplication | **NotImplemented** | No copying of current keyboard, timezone, currency, or regional settings into unattended setup. |
| Privacy/data-collection answers | **NotImplemented** | No unattended privacy-answer generation. |
| Windows To Go internal-disk protection | **NotImplemented** | No generated Windows To Go policy preventing access to internal disks. |
| Automatic BitLocker/device-encryption choice | **NotImplemented** | No option to disable automatic device encryption in Windows setup. |
| QoL changes | **NotImplemented** | No Rufus QoL selection for OneDrive, Outlook, Teams, Copilot, Fast Startup, and related Windows defaults. |
| Silent Windows installation | **NotImplemented / SafetyCompatibilityGap** | No silent erase/install mode, guard dialog, disk-isolation warning, or generated unattended answer file. This is intentionally not a simple parity checkbox because it can erase the first disk on the target PC. |
| S mode / SKU policy | **NotImplemented** | No expert S-Mode restriction or Windows SKU policy application. |
| Windows CA 2023 bootloaders | **NotImplemented / ValidationOnly** | No selection/application of Microsoft Windows CA 2023 signed bootloaders or target-compatibility warning. |
| Revoked UEFI bootloader detection | **NotImplemented / SafetyCompatibilityGap** | RustFus copies source EFI files but does not inspect revoked bootloaders or warn about Security Violation/Windows Recovery outcomes. |
| DBX update and SBAT/revocation data | **NotImplemented** | No downloadable/current DBX data, SBAT policy handling, or runtime UEFI revocation validation. |
| Windows version/build-aware policy | **Partial** | Required-file and EFI architecture checks exist, but no Rufus-equivalent Windows build detection, policy matrix, or version-aware WUE/bootloader decisions. |

### 4. Verification, formatting passes, and operational safety

| Rufus capability | RustFus status | Gap and evidence |
| --- | --- | --- |
| SHA-256 file round-trip verification | **Partial / Implemented** | RustFus hashes staged files while copying and reopens the FAT32 volume to check lengths and SHA-256. This is not whole-device image verification and does not authenticate the ISO or Windows signatures. |
| MD5/SHA-1/SHA-512 output/checks | **NotImplemented** | No user-selectable Rufus digest/reporting workflow. WIM SHA-1 fields may be structurally preserved by the splitter, but RustFus does not authenticate WIM resources. |
| Whole-device write verification | **NotImplemented / SafetyCompatibilityGap** | No sector-by-sector readback against a raw source image. |
| UEFI media validation | **NotImplemented / ValidationOnly** | No runtime bootloader extraction/validation. Existing source-file checks and automated fixtures are not firmware validation. |
| Quick format | **Implemented** | RustFus intentionally performs quick FAT32 formatting. It clearly does not claim secure erase. |
| Full format / zeroing | **NotImplemented** | No full-capacity zero pass or selectable overwrite pass count. |
| Bad-block scan | **NotImplemented** | No read/write bad-block test, retry policy, progress, or bad-block report. RustFus' capacity and filesystem checks are not media certification. |
| Extended label and icon files | **NotImplemented** | No `autorun.inf`, extended volume label, or icon-file creation. |
| Retry/error policy | **Partial** | RustFus has cancellation, cleanup, flush, detailed errors, and failed-media reporting. It does not have Rufus' configurable write retries or retry-cancellation behavior. |
| Cancellation and cleanup | **Partial / ValidationOnly** | The implementation has preflight staging, identity/capacity revalidation, exclusive access, cancellation, flush, and native cleanup. README/manual evidence still says raw USB, physical media, firmware, and Secure Boot validation have not been performed. |
| Device scan and filters | **Partial / Implemented** | Background discovery, removable filtering, and opt-in fixed USB disks exist. No Rufus Windows device metadata/logging breadth, VHD exposure, or full Windows disk policy matrix exists. |
| Automatic unmount/eject | **Intentional gap** | RustFus refuses mounted media and requires manual unmounting on Linux/macOS. Rufus' Windows workflow has native volume lock/dismount behavior. This is a platform/safety policy difference, not an accidental missing button. |
| Privilege elevation | **Intentional gap** | RustFus does not auto-elevate or run helper scripts. Rufus is Windows-only and uses the Windows elevation model. Do not represent this as a universal RustFus safety guarantee. |

### 5. UI, settings, distribution, and diagnostics

| Rufus capability | RustFus status | Gap and evidence |
| --- | --- | --- |
| Broad filesystem/boot dropdowns | **NotImplemented** | RustFus' current UI intentionally reduces boot choices to Non Bootable and Windows ISO, filesystem to FAT32, target to UEFI, and schemes to MBR/GPT. |
| Cluster-size control | **Partial / Implemented** | RustFus offers Auto, 512 B, 1, 2, 4, 8, 16, and 32 KiB. It does not offer per-filesystem Rufus defaults/allowed sizes. |
| Volume label | **Partial / Implemented** | FAT32 label validation and uppercasing exist. No extended label/icon behavior. |
| Advanced/expert mode and hidden Alt-key toggles | **NotImplemented** | No Rufus advanced mode, expert mode, force-large-FAT32, disable-VHD, ISO/Joliet/Rock Ridge toggles, preserve-timestamp, ignore-size, fake-drive, checksum, or related hidden controls. |
| Log window, save/clear log, persistent log | **Partial** | RustFus exposes operation status and copyable error details. It has no Rufus-style persistent English log, log dialog, save/clear controls, or cross-run diagnostics file. |
| Settings persistence / portable mode | **NotImplemented** | No `rufus.ini`-style portable settings behavior, saved language/options, or portable distribution mode. |
| Localization | **NotImplemented** | RustFus UI strings are currently embedded English. Rufus ships a large set of official translations and supports a local translation file mechanism. |
| Update checks | **NotImplemented** | No update policy/settings dialog or update check/download workflow. |
| Portable/standard signed Windows distribution | **Partial / OutOfScope** | RustFus builds raw native binaries for Linux, macOS, and Windows and has no installer/self-contained bundle or Rufus-style signed standard/portable Windows pair. |
| Windows x86 host build | **NotImplemented / OutOfScope** | Rufus publishes Windows x86, x64, and ARM64 binaries. RustFus' stated host matrix is x86_64 and ARM64; a 32-bit Windows ISO is not a 32-bit RustFus host build. |
| Native multi-OS advantage | **RustFus advantage** | Rufus is Windows-only. RustFus supports native Linux, macOS, and Windows code paths. This is not a gap and should not be traded away merely for parity. |

## Validation-only and evidence gaps

These items must not be reported as implemented merely because code or synthetic tests exist:

- No agent-performed raw USB formatting, full-capacity physical-media certification, physical UEFI boot, Secure Boot acceptance, or Windows installation is recorded.
- Synthetic sparse-image tests, ISO fixtures, WIM fixtures, SHA-256 checks, and cross-compilation demonstrate bounded behavior, not compatibility with every controller, firmware, Windows build, or ISO.
- Copying Microsoft-signed EFI files preserves source bytes but does not authenticate the ISO, validate certificate chains, detect revoked bootloaders, or prove Secure Boot acceptance.
- Reaching Windows Setup would not prove correct installation-target selection. RustFus' manual guide correctly stops before destructive installation.
- Current WIM support is an eligible standard v1.13 split path. It is not a general WIM/ESD converter, image-content verifier, decompressor, or DISM/wimlib runtime bridge.
- ISO parser support is intentionally bounded. A source being called “ISO” is not evidence that RustFus accepts its filesystem, allocations, symlinks, extents, names, or boot layout.

## Recommended priority grouping

### P0: safety and compatibility gates

- Preserve the current fail-closed behavior while adding any new mode.
- Define a raw-image/DD threat model, whole-device verification model, source mutation policy, target-size policy, retry policy, and recovery semantics before implementation.
- Add runtime UEFI-media validation and explicit revoked-bootloader/DBX/SBAT policy if Windows boot compatibility is expanded.
- Establish physical-media, UEFI, Secure Boot, and Windows Setup evidence before claiming parity.
- Do not add silent Windows installation or destructive unattended options without an equivalent multi-stage warning and test protocol.

### P1: core Rufus parity

- General image source model: ISO, raw/DD, compressed archives, VHD/VHDX, and FFU where platform support is defensible.
- FAT16, NTFS, exFAT, UDF, any deliberately supported ext/ReFS writers, and Rufus-compatible Large FAT32 defaults/policies.
- Linux and other ISO boot paths, including bootloader asset/version handling and BIOS/Legacy support.
- Windows To Go and Windows User Experience/unattended options, only with a separate safety design.
- Image and target compatibility inspection, source hashes, whole-device readback where applicable, and media bad-block/full-pass options.

### P2: convenience and distribution parity

- Localization and translation loading.
- Persistent logs, saved diagnostics, settings persistence, portable mode, and update checks.
- Extended label/icon files, advanced/expert controls, configurable retries, and Rufus-compatible cluster defaults.
- Fido/source-download integration and broad UI polish after the P0/P1 correctness work.

## Sources

All source claims were checked against the following URLs/files on 2026-10-27. GitHub material was obtained through authenticated `gh` CLI operations and a shallow clone of the official repository.

1. Rufus official site, current download/usage/features/changelog page: <https://rufus.ie/en/> (shows Rufus 4.15 dated 2026-06-30, Windows x86/x64/ARM64 downloads, supported languages, usage notes, and non-exhaustive supported ISO list).
2. Official Rufus repository: <https://github.com/pbatard/rufus>.
3. Official Rufus 4.15 release: <https://github.com/pbatard/rufus/releases/tag/v4.15> (release notes include RISC-V 64 UEFI:NTFS support, silent-install guards, compressed-image progress, UEFI media-validation fix, and Windows User Experience fixes).
4. Authenticated release evidence: `gh api repos/pbatard/rufus/releases/latest`, tag `v4.15`, published `2026-06-30T11:28:34Z`.
5. Rufus FAQ wiki: <https://github.com/pbatard/rufus.wiki/blob/master/FAQ.mediawiki> (portable settings, localization, Rufus scope, Windows To Go, and QoL behavior).
6. Rufus usage notes wiki: <https://github.com/pbatard/rufus.wiki/blob/master/Usage-Notes.mediawiki> (Syslinux/GRUB assets, Windows To Go restrictions, GPT/MSR/ESP details).
7. Rufus 4.15 source inspected at tag `v4.15`, especially `src/rufus.c`, `src/format.c`, `src/vhd.c`, `res/loc/rufus.loc`, filesystem/bootloader resources, and `ChangeLog.txt`.
8. RustFus current project documentation: [`README.md`](../README.md) and [`MANUAL_TESTING.md`](MANUAL_TESTING.md).
9. RustFus current implementation inspected in `crates/rustfus-core/src/model.rs`, `crates/rustfus-core/src/media/`, `crates/rustfus-core/src/platform/`, and `crates/rustfus-etc/src/view.rs`.
