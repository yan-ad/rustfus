use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

use crate::devices::DeviceInfo;
use crate::model::{BootSelection, IsoImage, PartitionScheme, TargetSystem};

use super::filesystem::{self, Region};
use super::layout::{self, Layout, LayoutScheme};
use super::native::NativeDisk;
use super::sector_io::SectorIo;
use super::windows_iso::{self, StagedFile};

const MIB: u64 = 1024 * 1024;
const MAX_FILES: usize = 200_000;

#[derive(Clone, Debug)]
pub struct WriteRequest {
    pub device: DeviceInfo,
    pub boot: BootSelection,
    pub image: Option<IsoImage>,
    pub scheme: PartitionScheme,
    pub target: TargetSystem,
    pub volume_label: String,
    pub cluster_size: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Preparing,
    Opening,
    Partitioning,
    Formatting,
    Copying,
    Verifying,
    Flushing,
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub phase: Phase,
    pub completed: u64,
    pub total: u64,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct WriteSummary {
    pub files_written: usize,
    pub bytes_written: u64,
    pub architecture: Option<String>,
}

/// Confirmation must be the exact selected device path. ISO mode installs a
/// Windows UEFI loader only, never a BIOS loader. Staging finishes before the
/// native device is opened; all filesystem writes use the exclusively held disk.
pub fn run(
    request: WriteRequest,
    confirmation: &str,
    cancel: &AtomicBool,
    report: impl Fn(Progress),
) -> Result<WriteSummary, String> {
    if confirmation != request.device.path || confirmation.is_empty() {
        return Err("Confirmation must exactly match the selected device path.".into());
    }
    validate_request(&request)?;
    check_cancel(cancel)?;
    emit(
        &report,
        Phase::Preparing,
        0,
        0,
        "Validating and staging source files",
    );
    let staged = if request.boot == BootSelection::Iso {
        Some(windows_iso::stage(
            request
                .image
                .as_ref()
                .ok_or("Select a Windows installer ISO first.")?,
            cancel,
            |completed, total| {
                emit(
                    &report,
                    Phase::Preparing,
                    completed,
                    total,
                    "Staging Windows installer",
                )
            },
        )?)
    } else {
        None
    };
    let files = staged.as_ref().map_or(&[][..], |s| s.files.as_slice());
    let total = preflight_files(files, cancel)?;
    check_fit(
        files,
        request.device.size_bytes.saturating_sub(2 * MIB),
        request.cluster_size,
    )?;
    check_cancel(cancel)?;
    emit(
        &report,
        Phase::Opening,
        0,
        0,
        "Locking and revalidating the selected disk",
    );
    check_cancel(cancel)?;
    let mut disk = NativeDisk::open(&request.device)?;
    let capacity = disk.capacity();
    let sector = disk.sector_size();
    let result = write_image(
        disk.file_mut(),
        capacity,
        sector,
        &request,
        files,
        cancel,
        &report,
        |raw| {
            raw.sync_all()
                .map_err(|e| format!("Synchronizing before verification: {e}"))
        },
    );
    emit(
        &report,
        Phase::Flushing,
        0,
        1,
        "Synchronizing device buffers",
    );
    // Neither cancellation nor a failed sync may bypass native cache flushing
    // and partition refresh while the exclusive disk guard is still held.
    let sync = disk.synchronize().map_err(incomplete);
    let finish = disk.finish().map_err(incomplete);
    complete_io(complete_io(result, sync), finish)?;
    check_cancel(cancel).map_err(incomplete)?;
    emit(&report, Phase::Flushing, 1, 1, "Device synchronized");
    Ok(WriteSummary {
        files_written: files.len(),
        bytes_written: total,
        architecture: staged.as_ref().map(|s| s.architecture.clone()),
    })
}

fn validate_request(request: &WriteRequest) -> Result<(), String> {
    filesystem::validate_label(&request.volume_label)?;
    if request.device.read_only {
        return Err("The selected device is read-only.".into());
    }
    if request.device.size_bytes < 34 * MIB {
        return Err("The selected device is too small for an aligned FAT32 partition.".into());
    }
    if let Some(cluster) = request.cluster_size
        && (!cluster.is_power_of_two() || !(512..=32768).contains(&cluster))
    {
        return Err("Cluster size must be a power of two between 512 bytes and 32 KiB.".into());
    }
    match request.boot {
        BootSelection::NonBootable => Ok(()),
        BootSelection::Iso if request.target == TargetSystem::Legacy => {
            Err("Legacy BIOS boot is not supported. Windows ISO writing supports UEFI only; choose BIOS or UEFI (UEFI mode).".into())
        }
        BootSelection::Iso if request.image.is_none() => Err("Select a Windows installer ISO first.".into()),
        BootSelection::Iso => Ok(()),
        BootSelection::MsDos | BootSelection::FreeDos => Err("DOS boot media is not supported.".into()),
    }
}

fn scheme(request: &WriteRequest) -> LayoutScheme {
    match request.scheme {
        PartitionScheme::Mbr => LayoutScheme::Mbr,
        PartitionScheme::Gpt => LayoutScheme::Gpt,
    }
}

fn emit(report: &impl Fn(Progress), phase: Phase, completed: u64, total: u64, detail: &str) {
    report(Progress {
        phase,
        completed,
        total,
        detail: detail.into(),
    });
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Write cancelled.".into())
    } else {
        Ok(())
    }
}

fn incomplete(error: String) -> String {
    format!("Media is incomplete and must be reformatted before use: {error}")
}

// Return an error other than Interrupted: std::io's retry loops must not swallow
// cancellation. Flush deliberately remains available for error-path cleanup.
struct CancelIo<'a, W> {
    inner: W,
    cancel: &'a AtomicBool,
}
impl<W> CancelIo<'_, W> {
    fn check(&self) -> io::Result<()> {
        check_cancel(self.cancel).map_err(io::Error::other)
    }
}
impl<W: Read> Read for CancelIo<'_, W> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        self.inner.read(bytes)
    }
}
impl<W: Write> Write for CancelIo<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.check()?;
        self.inner.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
impl<W: Seek> Seek for CancelIo<'_, W> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.check()?;
        self.inner.seek(from)
    }
}

fn valid_path(path: &str) -> Result<(), String> {
    if path.split('/').count() > 64 {
        return Err("Staged path exceeds 64 directory levels.".into());
    }
    for name in path.split('/') {
        if name.is_empty()
            || matches!(name, "." | "..")
            || name.ends_with([' ', '.'])
            || name.encode_utf16().count() > 255
            || name
                .chars()
                .any(|c| c < '\u{20}' || "\\:*?\"<>|".contains(c))
        {
            return Err(format!("Invalid FAT32 path: {path:?}"));
        }
    }
    Ok(())
}

fn preflight_files(files: &[StagedFile], cancel: &AtomicBool) -> Result<u64, String> {
    if files.len() > MAX_FILES {
        return Err("Too many staged files (maximum 200000).".into());
    }
    let mut paths = HashMap::new();
    let mut total = 0u64;
    for file in files {
        check_cancel(cancel)?;
        valid_path(&file.relative_path)?;
        let parts: Vec<_> = file.relative_path.split('/').collect();
        for end in 1..=parts.len() {
            let name = parts[..end].join("/");
            let key = name.to_uppercase();
            let is_file = end == parts.len();
            if let Some((old_name, old_file)) = paths.get(&key) {
                if old_name != &name || *old_file || is_file {
                    return Err(format!("Case-insensitive staged path collision: {name}"));
                }
            } else {
                paths.insert(key, (name, is_file));
            }
        }
        if paths.len() > MAX_FILES {
            return Err("Too many staged filesystem entries.".into());
        }
        if file.size_bytes > u64::from(u32::MAX) {
            return Err(format!(
                "File exceeds FAT32's 4 GiB limit: {}",
                file.relative_path
            ));
        }
        let metadata = std::fs::symlink_metadata(&file.source)
            .map_err(|e| format!("Cannot inspect {}: {e}", file.relative_path))?;
        if !metadata.is_file() || metadata.len() != file.size_bytes {
            return Err(format!(
                "Staged file changed or is not regular: {}",
                file.relative_path
            ));
        }
        total = total
            .checked_add(file.size_bytes)
            .ok_or("Staged byte count overflow")?;
    }
    Ok(total)
}

fn check_fit(files: &[StagedFile], available: u64, cluster: Option<u32>) -> Result<(), String> {
    if files.is_empty() {
        return Ok(());
    }
    // The default allocation unit is bounded above by 32 KiB. Reserve a full
    // cluster per directory and each entry's worst-case long-name slots too.
    let cluster = u64::from(cluster.unwrap_or(32768));
    let mut dirs = HashSet::new();
    let mut required = 0u64;
    for file in files {
        required = required
            .checked_add(file.size_bytes.div_ceil(cluster) * cluster)
            .ok_or("Space estimate overflow")?;
        let parts: Vec<_> = file.relative_path.split('/').collect();
        for end in 1..parts.len() {
            dirs.insert(parts[..end].join("/"));
        }
        let entry_bytes = (file.relative_path.encode_utf16().count() as u64).div_ceil(13) * 32 + 32;
        required = required
            .checked_add(entry_bytes)
            .ok_or("Directory size overflow")?;
    }
    required = required
        .checked_add((dirs.len() as u64 + 1) * (cluster + 1024))
        .and_then(|n| n.checked_add((available / 20).max(64 * MIB)))
        .ok_or("Space estimate overflow")?;
    if required > available {
        return Err(format!(
            "Staged installer does not fit: conservatively needs {required} bytes, {available} available."
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_image<W: Read + Write + Seek>(
    disk: &mut W,
    capacity: u64,
    sector: u32,
    request: &WriteRequest,
    files: &[StagedFile],
    cancel: &AtomicBool,
    report: &impl Fn(Progress),
    mut synchronize: impl FnMut(&mut W) -> Result<(), String>,
) -> Result<(), String> {
    validate_request(request)?;
    let total = preflight_files(files, cancel)?;
    let partition = layout::plan(capacity, sector, scheme(request))?;
    let geometry = filesystem::geometry(&partition, &request.volume_label, request.cluster_size)?;
    check_fit(files, geometry.available_bytes, Some(geometry.cluster_size))?;
    check_cancel(cancel)?;
    emit(
        report,
        Phase::Partitioning,
        0,
        1,
        "Writing partition table (existing data will be lost)",
    );
    check_cancel(cancel)?;
    let mut io = SectorIo::new(&mut *disk, capacity, sector).map_err(|e| e.to_string())?;
    let result = (|| {
        let mut checked = CancelIo {
            inner: &mut io,
            cancel,
        };
        layout::write_partition_table_with_efi(
            &mut checked,
            capacity,
            &partition,
            scheme(request),
            request.boot == BootSelection::Iso,
        )?;
        let tables = table_snapshot(&mut checked, capacity, &partition)?;
        emit(report, Phase::Formatting, 0, 1, "Quick-formatting FAT32");
        filesystem::format_fat32(
            &mut checked,
            &partition,
            &request.volume_label,
            request.cluster_size,
        )?;
        let hashes = copy_files(&mut checked, &partition, files, total, cancel, report)?;
        checked
            .flush()
            .map_err(|e| format!("Flushing file data: {e}"))?;
        Ok::<_, String>((hashes, tables))
    })();
    // Explicit even on cancellation/error; Drop never flushes SectorIo.
    let flush = io
        .flush()
        .map_err(|e| format!("Flushing sector cache: {e}"));
    drop(io);
    let (hashes, tables) = complete_io(result, flush).map_err(incomplete)?;
    synchronize(disk).map_err(incomplete)?;
    let mut fresh = SectorIo::new(disk, capacity, sector).map_err(|e| incomplete(e.to_string()))?;
    let result = (|| {
        let mut checked = CancelIo {
            inner: &mut fresh,
            cancel,
        };
        emit(
            report,
            Phase::Verifying,
            0,
            total,
            "Reopening and verifying every file",
        );
        if table_snapshot(&mut checked, capacity, &partition)? != tables {
            return Err("Partition table changed during filesystem writes.".into());
        }
        verify_files(
            &mut checked,
            &partition,
            &request.volume_label,
            &hashes,
            total,
            cancel,
            report,
        )
    })();
    let flush = fresh
        .flush()
        .map_err(|e| format!("Flushing verification cache: {e}"));
    complete_io(result, flush).map_err(incomplete)
}

fn complete_io<T>(result: Result<T, String>, flush: Result<(), String>) -> Result<T, String> {
    match (result, flush) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(flush)) => Err(format!("{error}; cleanup also failed: {flush}")),
    }
}

fn table_snapshot<W: Read + Seek>(
    disk: &mut W,
    capacity: u64,
    p: &Layout,
) -> Result<[u8; 32], String> {
    let mut hash = Sha256::new();
    let mut buffer = vec![0; MIB as usize];
    for (start, length) in [
        (0, p.start_bytes()?),
        (
            p.start_bytes()? + p.length_bytes()?,
            capacity - p.start_bytes()? - p.length_bytes()?,
        ),
    ] {
        disk.seek(SeekFrom::Start(start))
            .map_err(|e| format!("Reading partition table: {e}"))?;
        let mut left = length;
        while left > 0 {
            let n = left.min(buffer.len() as u64) as usize;
            disk.read_exact(&mut buffer[..n])
                .map_err(|e| format!("Reading partition table: {e}"))?;
            hash.update(&buffer[..n]);
            left -= n as u64;
        }
    }
    Ok(hash.finalize().into())
}

struct FileHash {
    path: String,
    size: u64,
    hash: [u8; 32],
}

fn copy_files<W: Read + Write + Seek>(
    disk: &mut W,
    p: &Layout,
    files: &[StagedFile],
    total: u64,
    cancel: &AtomicBool,
    report: &impl Fn(Progress),
) -> Result<Vec<FileHash>, String> {
    let region =
        Region::new(disk, p.start_bytes()?, p.length_bytes()?).map_err(|e| e.to_string())?;
    let fs = fatfs::FileSystem::new(region, fatfs::FsOptions::new())
        .map_err(|e| format!("Mounting new FAT32: {e}"))?;
    let result = (|| {
        let stats = fs
            .stats()
            .map_err(|e| format!("Checking free clusters: {e}"))?;
        let expected = filesystem::geometry(p, "", Some(fs.cluster_size()))?;
        if stats.total_clusters() != expected.data_clusters
            || u64::from(stats.free_clusters()) * u64::from(fs.cluster_size())
                != expected.available_bytes
        {
            return Err("Formatted FAT32 free space differs from preflight geometry.".into());
        }
        check_fit(
            files,
            u64::from(stats.free_clusters()) * u64::from(fs.cluster_size()),
            Some(fs.cluster_size()),
        )?;
        let root = fs.root_dir();
        if root.iter().next().is_some() {
            return Err("New FAT32 filesystem is not empty.".into());
        }
        let mut hashes = Vec::with_capacity(files.len());
        let mut buffer = vec![0; MIB as usize];
        let mut completed = 0;
        emit(report, Phase::Copying, 0, total, "Copying staged files");
        let mut directories = HashSet::new();
        for file in files {
            check_cancel(cancel)?;
            let parts: Vec<_> = file.relative_path.split('/').collect();
            for end in 1..parts.len() {
                let path = parts[..end].join("/");
                if directories.insert(path.clone()) {
                    root.create_dir(&path)
                        .map_err(|e| format!("Creating directory {path}: {e}"))?;
                }
            }
            let mut input = File::open(&file.source)
                .map_err(|e| format!("Opening staged {}: {e}", file.relative_path))?;
            let mut output = root
                .create_file(&file.relative_path)
                .map_err(|e| format!("Creating {}: {e}", file.relative_path))?;
            let mut hash = Sha256::new();
            let mut left = file.size_bytes;
            while left > 0 {
                check_cancel(cancel)?;
                let n = left.min(buffer.len() as u64) as usize;
                input
                    .read_exact(&mut buffer[..n])
                    .map_err(|e| format!("Reading staged {}: {e}", file.relative_path))?;
                output
                    .write_all(&buffer[..n])
                    .map_err(|e| format!("Writing {}: {e}", file.relative_path))?;
                hash.update(&buffer[..n]);
                left -= n as u64;
                completed += n as u64;
                emit(
                    report,
                    Phase::Copying,
                    completed,
                    total,
                    &file.relative_path,
                );
            }
            if input.read(&mut [0; 1]).map_err(|e| e.to_string())? != 0 {
                return Err(format!(
                    "Staged file grew during copy: {}",
                    file.relative_path
                ));
            }
            output
                .flush()
                .map_err(|e| format!("Flushing {}: {e}", file.relative_path))?;
            hashes.push(FileHash {
                path: file.relative_path.clone(),
                size: file.size_bytes,
                hash: hash.finalize().into(),
            });
        }
        check_cancel(cancel)?;
        Ok(hashes)
    })();
    let unmount = fs
        .unmount()
        .map_err(|e| format!("Unmounting written FAT32: {e}"));
    complete_io(result, unmount)
}

fn verify_files<W: Read + Write + Seek>(
    disk: &mut W,
    p: &Layout,
    label: &str,
    hashes: &[FileHash],
    total: u64,
    cancel: &AtomicBool,
    report: &impl Fn(Progress),
) -> Result<(), String> {
    let region =
        Region::new(disk, p.start_bytes()?, p.length_bytes()?).map_err(|e| e.to_string())?;
    let fs = fatfs::FileSystem::new(region, fatfs::FsOptions::new())
        .map_err(|e| format!("Reopening FAT32: {e}"))?;
    let result = (|| {
        if fs.fat_type() != fatfs::FatType::Fat32 || fs.volume_label() != label.to_ascii_uppercase()
        {
            return Err("FAT32 type or volume label verification failed.".into());
        }
        let root = fs.root_dir();
        if hashes.is_empty() && root.iter().next().is_some() {
            return Err("Non-bootable filesystem unexpectedly contains files.".into());
        }
        let mut completed = 0;
        let mut buffer = vec![0; MIB as usize];
        for expected in hashes {
            check_cancel(cancel)?;
            let mut file = root
                .open_file(&expected.path)
                .map_err(|e| format!("Verifying {}: {e}", expected.path))?;
            if file.seek(SeekFrom::End(0)).map_err(|e| e.to_string())? != expected.size {
                return Err(format!("Size verification failed: {}", expected.path));
            }
            file.rewind().map_err(|e| e.to_string())?;
            let mut hash = Sha256::new();
            let mut left = expected.size;
            while left > 0 {
                check_cancel(cancel)?;
                let n = left.min(buffer.len() as u64) as usize;
                file.read_exact(&mut buffer[..n])
                    .map_err(|e| format!("Verifying {}: {e}", expected.path))?;
                hash.update(&buffer[..n]);
                completed += n as u64;
                left -= n as u64;
                emit(report, Phase::Verifying, completed, total, &expected.path);
            }
            let actual: [u8; 32] = hash.finalize().into();
            if actual != expected.hash {
                return Err(format!("SHA-256 verification failed: {}", expected.path));
            }
            emit(report, Phase::Verifying, completed, total, &expected.path);
        }
        check_cancel(cancel)
    })();
    let unmount = fs
        .unmount()
        .map_err(|e| format!("Unmounting verified FAT32: {e}"));
    complete_io(result, unmount)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn request() -> WriteRequest {
        WriteRequest {
            device: DeviceInfo {
                id: "never-a-real-device".into(),
                path: "/not/a/device".into(),
                name: "Test image".into(),
                serial: None,
                size_bytes: 128 * MIB,
                removable: true,
                read_only: false,
            },
            boot: BootSelection::NonBootable,
            image: None,
            scheme: PartitionScheme::Mbr,
            target: TargetSystem::BiosOrUefi,
            volume_label: "TEST USB".into(),
            cluster_size: None,
        }
    }

    #[test]
    fn validation_denies_without_discovery_or_open() {
        let cancel = AtomicBool::new(false);
        let req = request();
        assert!(
            run(req.clone(), "wrong", &cancel, |_| panic!(
                "unexpected progress"
            ))
            .unwrap_err()
            .contains("Confirmation")
        );
        let mut req = req;
        req.boot = BootSelection::Iso;
        req.target = TargetSystem::Legacy;
        assert!(
            run(req.clone(), &req.device.path.clone(), &cancel, |_| panic!(
                "unexpected progress"
            ))
            .unwrap_err()
            .contains("Legacy BIOS")
        );
        req.boot = BootSelection::FreeDos;
        assert!(validate_request(&req).unwrap_err().contains("DOS"));
        req.boot = BootSelection::NonBootable;
        assert!(validate_request(&req).is_ok());
        req.device.read_only = true;
        assert!(validate_request(&req).is_err());
    }

    #[test]
    fn sparse_images_format_copy_and_reopen_every_file() {
        for scheme in [PartitionScheme::Mbr, PartitionScheme::Gpt] {
            let dir = tempfile::tempdir().unwrap();
            let data = vec![0x5a; MIB as usize + 73];
            std::fs::write(dir.path().join("payload"), &data).unwrap();
            std::fs::write(dir.path().join("empty"), b"").unwrap();
            let files = vec![
                StagedFile {
                    source: dir.path().join("payload"),
                    relative_path: "EFI/BOOT/payload.bin".into(),
                    size_bytes: data.len() as u64,
                },
                StagedFile {
                    source: dir.path().join("empty"),
                    relative_path: "empty.txt".into(),
                    size_bytes: 0,
                },
            ];
            let mut req = request();
            req.scheme = scheme;
            let mut disk = tempfile::tempfile().unwrap();
            disk.set_len(req.device.size_bytes).unwrap();
            let phases = RefCell::new(Vec::new());
            let synchronized = Cell::new(false);
            write_image(
                &mut disk,
                req.device.size_bytes,
                512,
                &req,
                &files,
                &AtomicBool::new(false),
                &|p| {
                    if p.phase == Phase::Verifying {
                        assert!(synchronized.get(), "verification preceded raw sync");
                    }
                    phases.borrow_mut().push((p.phase, p.detail));
                },
                |raw| {
                    raw.sync_all().map_err(|e| e.to_string())?;
                    synchronized.set(true);
                    Ok(())
                },
            )
            .unwrap();
            assert!(
                phases
                    .borrow()
                    .iter()
                    .any(|(p, name)| *p == Phase::Verifying && name == "empty.txt")
            );
            let p = layout::plan(req.device.size_bytes, 512, super::scheme(&req)).unwrap();
            let region = Region::new(
                &mut disk,
                p.start_bytes().unwrap(),
                p.length_bytes().unwrap(),
            )
            .unwrap();
            let fs = fatfs::FileSystem::new(region, fatfs::FsOptions::new()).unwrap();
            let mut actual = Vec::new();
            fs.root_dir()
                .open_file("EFI/BOOT/payload.bin")
                .unwrap()
                .read_to_end(&mut actual)
                .unwrap();
            assert_eq!(actual, data);
            fs.unmount().unwrap();
            // The same pipeline with no files produces a genuinely empty volume.
            req.target = TargetSystem::Legacy;
            write_image(
                &mut disk,
                req.device.size_bytes,
                512,
                &req,
                &[],
                &AtomicBool::new(false),
                &|_| {},
                |raw| raw.sync_all().map_err(|e| e.to_string()),
            )
            .unwrap();
        }
    }

    #[test]
    fn cancellation_during_format_is_incomplete_and_preflight_is_untouched() {
        let req = request();
        let mut disk = tempfile::tempfile().unwrap();
        disk.set_len(req.device.size_bytes).unwrap();
        disk.write_all(b"unchanged").unwrap();
        let cancel = AtomicBool::new(false);
        let mut bad = req.clone();
        bad.cluster_size = Some(32768);
        assert!(
            write_image(
                &mut disk,
                req.device.size_bytes,
                512,
                &bad,
                &[],
                &cancel,
                &|_| {},
                |_| Ok(()),
            )
            .is_err()
        );
        disk.rewind().unwrap();
        let mut marker = [0; 9];
        disk.read_exact(&mut marker).unwrap();
        assert_eq!(&marker, b"unchanged");
        let error = write_image(
            &mut disk,
            req.device.size_bytes,
            512,
            &req,
            &[],
            &cancel,
            &|p| {
                if p.phase == Phase::Formatting {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(
            error.contains("incomplete") && error.contains("cancelled"),
            "{error}"
        );
    }

    #[test]
    fn exact_free_space_rejection_preserves_partition_marker() {
        let req = request();
        let p = layout::plan(req.device.size_bytes, 512, LayoutScheme::Mbr).unwrap();
        let geometry = filesystem::geometry(&p, &req.volume_label, None).unwrap();
        let source = tempfile::NamedTempFile::new().unwrap();
        let size = p.length_bytes().unwrap() - 64 * MIB - MIB;
        source.as_file().set_len(size).unwrap();
        let files = [StagedFile {
            source: source.path().into(),
            relative_path: "payload.bin".into(),
            size_bytes: size,
        }];
        assert!(check_fit(&files, p.length_bytes().unwrap(), None).is_ok());
        assert!(
            check_fit(
                &files,
                geometry.available_bytes,
                Some(geometry.cluster_size)
            )
            .is_err()
        );
        let mut disk = tempfile::tempfile().unwrap();
        disk.set_len(req.device.size_bytes).unwrap();
        disk.write_all(b"unchanged").unwrap();
        let error = write_image(
            &mut disk,
            req.device.size_bytes,
            512,
            &req,
            &files,
            &AtomicBool::new(false),
            &|_| panic!("preflight must not start partitioning"),
            |_| panic!("preflight must not reach verification sync"),
        )
        .unwrap_err();
        assert!(error.contains("does not fit"), "{error}");
        disk.rewind().unwrap();
        let mut marker = [0; 9];
        disk.read_exact(&mut marker).unwrap();
        assert_eq!(&marker, b"unchanged");
    }

    #[test]
    fn failed_sync_prevents_verification_and_cleanup_preserves_both_errors() {
        let req = request();
        let mut disk = tempfile::tempfile().unwrap();
        disk.set_len(req.device.size_bytes).unwrap();
        let error = write_image(
            &mut disk,
            req.device.size_bytes,
            512,
            &req,
            &[],
            &AtomicBool::new(false),
            &|p| assert_ne!(p.phase, Phase::Verifying),
            |_| Err("injected sync failure".into()),
        )
        .unwrap_err();
        assert!(error.contains("incomplete") && error.contains("injected sync failure"));
        let error =
            complete_io::<()>(Err("copy failed".into()), Err("unmount failed".into())).unwrap_err();
        assert!(error.contains("copy failed") && error.contains("unmount failed"));
    }

    #[test]
    fn fit_and_path_preflight_include_empty_files_and_collisions() {
        for bad in ["../evil", "/absolute", "a//b", "a\\b", "a/./b", "a?", "a."] {
            assert!(valid_path(bad).is_err(), "{bad}");
        }
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::write(&source, b"").unwrap();
        let files = vec![
            StagedFile {
                source: source.clone(),
                relative_path: "a/file".into(),
                size_bytes: 0,
            },
            StagedFile {
                source,
                relative_path: "A/other".into(),
                size_bytes: 0,
            },
        ];
        assert!(preflight_files(&files, &AtomicBool::new(false)).is_err());
        assert!(check_fit(&files, MIB, None).is_err());
    }

    #[test]
    fn verification_rejects_same_size_corruption() {
        let req = request();
        let mut disk = tempfile::tempfile().unwrap();
        disk.set_len(req.device.size_bytes).unwrap();
        let p = layout::plan(req.device.size_bytes, 512, LayoutScheme::Mbr).unwrap();
        filesystem::format_fat32(&mut disk, &p, &req.volume_label, None).unwrap();
        {
            let region = Region::new(
                &mut disk,
                p.start_bytes().unwrap(),
                p.length_bytes().unwrap(),
            )
            .unwrap();
            let fs = fatfs::FileSystem::new(region, fatfs::FsOptions::new()).unwrap();
            fs.root_dir()
                .create_file("file")
                .unwrap()
                .write_all(b"wrong")
                .unwrap();
            fs.unmount().unwrap();
        }
        let expected = FileHash {
            path: "file".into(),
            size: 5,
            hash: Sha256::digest(b"right").into(),
        };
        let error = verify_files(
            &mut disk,
            &p,
            &req.volume_label,
            &[expected],
            5,
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap_err();
        assert!(error.contains("SHA-256"));
    }
}
