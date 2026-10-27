use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use isomage::{TreeNode, cat_node};
use tempfile::{TempDir, tempdir};

const MAX_ENTRIES: usize = 200_000;
const MAX_DEPTH: usize = 64;
const MAX_WIM_PART: u64 = 3_800_000_000;
const U32_MAX: u64 = u32::MAX as u64;

pub struct StagedWindows {
    _tempdir: TempDir,
    pub root: PathBuf,
    pub files: Vec<StagedFile>,
    pub total_bytes: u64,
    pub architecture: String,
}

fn validate_windows_tree(tree: &TreeNode) -> Result<(), String> {
    fn lookup<'a>(mut node: &'a TreeNode, path: &str) -> Option<&'a TreeNode> {
        for part in path.split('/') {
            node = node
                .children
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(part))?;
        }
        (!node.is_directory && node.size != 0).then_some(node)
    }
    if lookup(tree, "setup.exe").is_none() || lookup(tree, "sources/boot.wim").is_none() {
        return Err("Only standard Windows installer images with setup.exe and sources/boot.wim are supported.".into());
    }
    if [
        "sources/install.wim",
        "sources/install.esd",
        "sources/install.swm",
    ]
    .iter()
    .all(|p| lookup(tree, p).is_none())
    {
        return Err("Windows image has no install.wim, install.esd, or install.swm.".into());
    }
    if [
        "EFI/BOOT/BOOTX64.EFI",
        "EFI/BOOT/BOOTAA64.EFI",
        "EFI/BOOT/BOOTIA32.EFI",
    ]
    .iter()
    .all(|p| lookup(tree, p).is_none_or(|n| n.size < 64))
    {
        return Err("Windows image has no supported UEFI boot file of at least 64 bytes.".into());
    }
    Ok(())
}

pub struct StagedFile {
    pub relative_path: String,
    pub size_bytes: u64,
    pub source: PathBuf,
}

pub fn stage(
    image: &crate::model::IsoImage,
    cancel: &AtomicBool,
    progress: impl Fn(u64, u64),
) -> Result<StagedWindows, String> {
    let mut input = File::open(&image.path).map_err(|e| format!("Cannot open image: {e}"))?;
    let before = snapshot(&input, image)?;

    check_cancel(cancel)?;
    let tree = super::iso_reader::parse(&mut input, image.size_bytes).map_err(|error| {
        format!("Image is not a supported, bounded ISO9660/UDF filesystem: {error}")
    })?;
    let extraction_total = validate_tree(&tree, image.size_bytes)?;
    validate_windows_tree(&tree)?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Staging cancelled.".into());
    }

    let tempdir = tempdir().map_err(|e| format!("Cannot create staging directory: {e}"))?;
    let root = tempdir.path().to_path_buf();
    let mut files = Vec::new();
    let mut copied = 0u64;
    extract_tree(
        &mut input,
        &tree,
        Path::new(""),
        &mut ExtractionContext {
            root: &root,
            total: extraction_total,
            cancel,
            progress: &progress,
            copied: &mut copied,
            files: &mut files,
        },
    )?;
    verify_unchanged(&input, image, before)?;

    let setup = find_file(&files, "setup.exe").ok_or("Windows image is missing setup.exe.")?;
    if setup.size_bytes == 0 {
        return Err("Windows setup.exe is empty.".into());
    }
    let boot = find_file(&files, "sources/boot.wim")
        .ok_or("Windows image is missing sources/boot.wim.")?;
    if boot.size_bytes == 0 {
        return Err("sources/boot.wim is empty.".into());
    }
    if boot.size_bytes > U32_MAX {
        return Err("sources/boot.wim is too large for the target filesystem.".into());
    }
    let install = find_file(&files, "sources/install.wim");
    let esd = find_file(&files, "sources/install.esd");
    let swm = validate_swm(&files)?;
    if install.is_none() && esd.is_none() && !swm {
        return Err(
            "Windows image has no sources/install.wim, install.esd, or install*.swm.".into(),
        );
    }
    if let Some(esd) = esd
        && esd.size_bytes > U32_MAX
    {
        return Err(
            "sources/install.esd exceeds 4 GiB and must be recompressed before writing.".into(),
        );
    }
    if let Some(wim) = install
        && wim.size_bytes > U32_MAX
    {
        let input_path = wim.source.clone();
        if swm {
            return Err(
                "Image contains both an oversized install.wim and split install parts.".into(),
            );
        }
        let output = input_path
            .parent()
            .ok_or("install.wim has no parent directory.")?;
        super::wim::split(&input_path, output, MAX_WIM_PART, cancel, &progress)
            .map_err(|e| format!("Cannot split install.wim: {e}"))?;
        fs::remove_file(&input_path)
            .map_err(|e| format!("Cannot remove staged install.wim: {e}"))?;
        files.retain(|f| !f.relative_path.eq_ignore_ascii_case("sources/install.wim"));
        collect_new_files(&root, Path::new(""), &mut files)?;
        if !validate_swm(&files)? {
            return Err("WIM split produced no install.swm parts.".into());
        }
    }

    let architecture = detect_architecture(&files, &root)?;
    check_cancel(cancel)?;
    verify_unchanged(&input, image, before)?;
    let total_bytes = files
        .iter()
        .try_fold(0u64, |sum, f| sum.checked_add(f.size_bytes))
        .ok_or("Staged size overflow.")?;
    progress(total_bytes, total_bytes);
    Ok(StagedWindows {
        _tempdir: tempdir,
        root,
        files,
        total_bytes,
        architecture,
    })
}

fn snapshot(
    file: &File,
    image: &crate::model::IsoImage,
) -> Result<(u64, Option<SystemTime>), String> {
    let m = file
        .metadata()
        .map_err(|e| format!("Cannot inspect image: {e}"))?;
    if !m.is_file() || m.len() != image.size_bytes {
        return Err("ISO changed or is not a regular file.".into());
    }
    let modified = m
        .modified()
        .map_err(|e| format!("Cannot read image modification time: {e}"))?;
    Ok((m.len(), Some(modified)))
}

fn verify_unchanged(
    file: &File,
    image: &crate::model::IsoImage,
    before: (u64, Option<SystemTime>),
) -> Result<(), String> {
    let after = snapshot(file, image)?;
    if after != before {
        Err("ISO changed while it was being staged.".into())
    } else {
        Ok(())
    }
}

fn validate_tree(root: &TreeNode, image_size: u64) -> Result<u64, String> {
    if !root.is_directory {
        return Err("ISO root is not a directory.".into());
    }
    let mut count = 0usize;
    let mut total = 0;
    validate_node(root, "", 0, image_size, &mut count, &mut total)?;
    Ok(total)
}

fn validate_node(
    node: &TreeNode,
    path: &str,
    depth: usize,
    image_size: u64,
    count: &mut usize,
    total: &mut u64,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("ISO directory nesting exceeds 64 levels.".into());
    }
    if depth != 0 {
        validate_name(&node.name)?;
        *count += 1;
        if *count > MAX_ENTRIES {
            return Err("ISO contains more than 200000 entries.".into());
        }
    }
    if !node.is_directory {
        if !node.children.is_empty() {
            return Err("File entry contains child entries.".into());
        }
        let size = node.file_length.ok_or("File has no length.")?;
        if size != node.size {
            return Err("File size disagrees with its extent length.".into());
        }
        let location = node
            .file_location
            .ok_or_else(|| format!("File {} has no extent.", node.name))?;
        let end = location.checked_add(size).ok_or("File extent overflow.")?;
        if end > image_size {
            return Err(format!("File {} extends beyond the image.", node.name));
        }
        if size > U32_MAX && !path.eq_ignore_ascii_case("sources/install.wim") {
            return Err(if path.eq_ignore_ascii_case("sources/install.esd") {
                "sources/install.esd exceeds FAT32's limit and must be recompressed.".into()
            } else {
                format!("File {path} exceeds FAT32's 4 GiB file limit.")
            });
        }
        *total = total.checked_add(size).ok_or("ISO total size overflow.")?;
    }
    let mut names = HashSet::new();
    for child in &node.children {
        if !names.insert(child.name.to_uppercase()) {
            return Err(format!("Case-insensitive duplicate entry: {}", child.name));
        }
        let next = if path.is_empty() {
            child.name.clone()
        } else {
            format!("{path}/{}", child.name)
        };
        validate_node(child, &next, depth + 1, image_size, count, total)?;
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), String> {
    if name == "."
        || name == ".."
        || name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.contains(':')
    {
        return Err(format!("Illegal Windows filename: {name:?}"));
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return Err(format!(
            "Windows filename has an illegal trailing character: {name:?}"
        ));
    }
    if name.encode_utf16().count() > 255 {
        return Err(format!(
            "Windows filename is longer than 255 UTF-16 units: {name:?}"
        ));
    }
    if name.chars().any(|c| c <= '\u{1f}' || "<>\"|?*".contains(c)) {
        return Err(format!("Illegal Windows filename: {name:?}"));
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches(' ')
        .to_uppercase();
    if matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "CLOCK$"
            | "CONIN$"
            | "CONOUT$"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
            | "COM¹"
            | "COM²"
            | "COM³"
            | "LPT¹"
            | "LPT²"
            | "LPT³"
    ) {
        return Err(format!("Windows reserved filename: {name:?}"));
    }
    Ok(())
}

struct ExtractionContext<'a, F> {
    root: &'a Path,
    total: u64,
    cancel: &'a AtomicBool,
    progress: &'a F,
    copied: &'a mut u64,
    files: &'a mut Vec<StagedFile>,
}

fn extract_tree<R: Read + std::io::Seek>(
    input: &mut R,
    node: &TreeNode,
    rel: &Path,
    context: &mut ExtractionContext<'_, impl Fn(u64, u64)>,
) -> Result<(), String> {
    for child in &node.children {
        if context.cancel.load(Ordering::Relaxed) {
            return Err("Staging cancelled.".into());
        }
        let next = rel.join(&child.name);
        let destination = context.root.join(&next);
        if child.is_directory {
            fs::create_dir(&destination)
                .map_err(|e| format!("Cannot create {}: {e}", next.display()))?;
            extract_tree(input, child, &next, context)?;
        } else {
            let size = child.file_length.unwrap_or(child.size);
            let mut out = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .map_err(|e| format!("Cannot create {}: {e}", next.display()))?;
            let mut writer = CopyProgress {
                out: &mut out,
                copied: context.copied,
                total: context.total,
                cancel: context.cancel,
                progress: context.progress,
            };
            cat_node(input, child, &mut writer)
                .map_err(|e| format!("Cannot extract {}: {e}", next.display()))?;
            out.flush()
                .map_err(|e| format!("Cannot flush {}: {e}", next.display()))?;
            context.files.push(StagedFile {
                relative_path: next.to_string_lossy().replace('\\', "/"),
                size_bytes: size,
                source: destination,
            });
        }
    }
    Ok(())
}

fn find_file<'a>(files: &'a [StagedFile], path: &str) -> Option<&'a StagedFile> {
    files
        .iter()
        .find(|f| f.relative_path.eq_ignore_ascii_case(path))
}

fn collect_new_files(root: &Path, rel: &Path, files: &mut Vec<StagedFile>) -> Result<(), String> {
    for e in fs::read_dir(root.join(rel)).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let p = e.path();
        let r = rel.join(e.file_name());
        let metadata = fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
        if metadata.is_dir() {
            collect_new_files(root, &r, files)?;
        } else if !metadata.is_file() {
            return Err("Staged tree contains a symlink or special file.".into());
        } else if !files.iter().any(|f| f.source == p) {
            if metadata.len() > U32_MAX {
                return Err("Split WIM part exceeds FAT32 file size limit.".into());
            }
            files.push(StagedFile {
                relative_path: r.to_string_lossy().replace('\\', "/"),
                size_bytes: metadata.len(),
                source: p,
            });
        }
    }
    Ok(())
}

fn detect_architecture(files: &[StagedFile], _root: &Path) -> Result<String, String> {
    let mut architectures = Vec::new();
    for (name, machine, architecture) in [
        ("EFI/BOOT/BOOTX64.EFI", 0x8664, "x86_64"),
        ("EFI/BOOT/BOOTAA64.EFI", 0xaa64, "arm64"),
        ("EFI/BOOT/BOOTIA32.EFI", 0x014c, "x86"),
    ] {
        if let Some(f) = find_file(files, name) {
            if f.size_bytes < 64 {
                return Err("Windows EFI boot file is too small.".into());
            }
            let mut input = File::open(&f.source).map_err(|e| e.to_string())?;
            let mut b = [0u8; 64];
            input.read_exact(&mut b).map_err(|e| e.to_string())?;
            let offset = u32::from_le_bytes(b[60..64].try_into().unwrap()) as u64;
            if &b[..2] != b"MZ"
                || offset < 64
                || offset.checked_add(24).is_none_or(|end| end > f.size_bytes)
            {
                return Err(format!("{name} is not a valid PE image."));
            }
            input
                .seek(SeekFrom::Start(offset))
                .map_err(|e| e.to_string())?;
            let mut pe = [0u8; 24];
            input.read_exact(&mut pe).map_err(|e| e.to_string())?;
            if &pe[..4] != b"PE\0\0" || u16::from_le_bytes([pe[4], pe[5]]) != machine {
                return Err(format!(
                    "{name} has an invalid PE signature or architecture."
                ));
            }
            architectures.push(architecture);
        }
    }
    if architectures.is_empty() {
        Err("Windows image has no supported UEFI boot file.".into())
    } else {
        Ok(architectures.join("+"))
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Staging cancelled.".into())
    } else {
        Ok(())
    }
}

struct CopyProgress<'a, F> {
    out: &'a mut File,
    copied: &'a mut u64,
    total: u64,
    cancel: &'a AtomicBool,
    progress: &'a F,
}

impl<F: Fn(u64, u64)> Write for CopyProgress<'_, F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("Staging cancelled."));
        }
        let n = self.out.write(bytes)?;
        *self.copied = self
            .copied
            .checked_add(n as u64)
            .ok_or_else(|| io::Error::other("Progress overflow."))?;
        (self.progress)(*self.copied, self.total);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

fn validate_swm(files: &[StagedFile]) -> Result<bool, String> {
    let mut parts = Vec::new();
    for file in files {
        let path = file.relative_path.to_ascii_lowercase();
        if let Some(suffix) = path
            .strip_prefix("sources/install")
            .and_then(|s| s.strip_suffix(".swm"))
        {
            let part = if suffix.is_empty() {
                1
            } else {
                suffix
                    .parse::<usize>()
                    .map_err(|_| "Invalid install*.swm part name.")?
            };
            if part == 0 || (part == 1 && !suffix.is_empty()) || file.size_bytes == 0 {
                return Err("Invalid split Windows image part.".into());
            }
            parts.push(part);
        }
    }
    parts.sort_unstable();
    if parts.iter().enumerate().any(|(i, n)| *n != i + 1) {
        return Err("Split Windows image is missing an install*.swm part.".into());
    }
    Ok(!parts.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    use sha2::{Digest, Sha256};

    const SECTOR: usize = 2048;

    fn both_endian_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        bytes[offset + 2..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn both_endian_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        bytes[offset + 4..offset + 8].copy_from_slice(&value.to_be_bytes());
    }

    fn iso_record(name: &[u8], sector: u32, size: usize, directory: bool) -> Vec<u8> {
        let length = (33 + name.len() + 1) & !1;
        let mut record = vec![0; length];
        record[0] = u8::try_from(length).unwrap();
        both_endian_u32(&mut record, 2, sector);
        both_endian_u32(&mut record, 10, u32::try_from(size).unwrap());
        record[18..25].copy_from_slice(&[126, 1, 1, 0, 0, 0, 0]);
        record[25] = if directory { 2 } else { 0 };
        both_endian_u16(&mut record, 28, 1);
        record[32] = u8::try_from(name.len()).unwrap();
        record[33..33 + name.len()].copy_from_slice(name);
        record
    }

    fn iso_directory(bytes: &mut [u8], sector: usize, parent: u32, records: &[Vec<u8>]) {
        let directory = &mut bytes[sector * SECTOR..(sector + 1) * SECTOR];
        directory.fill(0);
        let dot = iso_record(&[0], u32::try_from(sector).unwrap(), SECTOR, true);
        let dotdot = iso_record(&[1], parent, SECTOR, true);
        let mut offset = 0;
        for record in [&dot, &dotdot].into_iter().chain(records) {
            directory[offset..offset + record.len()].copy_from_slice(record);
            offset += record.len();
        }
    }

    fn windows_iso_files() -> [(&'static str, Vec<u8>); 4] {
        let mut loader = vec![0; 88];
        loader[..2].copy_from_slice(b"MZ");
        loader[60..64].copy_from_slice(&64u32.to_le_bytes());
        loader[64..68].copy_from_slice(b"PE\0\0");
        loader[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
        [
            ("SETUP.EXE", b"test setup executable".to_vec()),
            (
                "sources/BOOT.WIM",
                (0..5000).map(|i| (i % 251) as u8).collect(),
            ),
            ("sources/INSTALL.ESD", b"test install image".to_vec()),
            ("EFI/BOOT/BOOTX64.EFI", loader),
        ]
    }

    fn windows_iso() -> Vec<u8> {
        let mut bytes = vec![0; 64 * SECTOR];
        let pvd = &mut bytes[16 * SECTOR..17 * SECTOR];
        pvd[..7].copy_from_slice(b"\x01CD001\x01");
        pvd[8..72].fill(b' ');
        pvd[40..52].copy_from_slice(b"RUSTFUS_TEST");
        both_endian_u32(pvd, 80, 64);
        both_endian_u16(pvd, 120, 1);
        both_endian_u16(pvd, 124, 1);
        both_endian_u16(pvd, 128, SECTOR as u16);
        both_endian_u32(pvd, 132, 50);
        pvd[140..144].copy_from_slice(&18u32.to_le_bytes());
        pvd[148..152].copy_from_slice(&19u32.to_be_bytes());
        pvd[156..190].copy_from_slice(&iso_record(&[0], 20, SECTOR, true));
        pvd[881] = 1;
        bytes[17 * SECTOR..17 * SECTOR + 7].copy_from_slice(b"\xffCD001\x01");

        for (sector, big_endian) in [(18, false), (19, true)] {
            let mut offset = sector * SECTOR;
            for (name, extent, parent) in [
                (&[0][..], 20u32, 1u16),
                (&b"EFI"[..], 21, 1),
                (&b"sources"[..], 23, 1),
                (&b"BOOT"[..], 22, 2),
            ] {
                bytes[offset] = u8::try_from(name.len()).unwrap();
                let location = if big_endian {
                    extent.to_be_bytes()
                } else {
                    extent.to_le_bytes()
                };
                let parent = if big_endian {
                    parent.to_be_bytes()
                } else {
                    parent.to_le_bytes()
                };
                bytes[offset + 2..offset + 6].copy_from_slice(&location);
                bytes[offset + 6..offset + 8].copy_from_slice(&parent);
                bytes[offset + 8..offset + 8 + name.len()].copy_from_slice(name);
                offset += 8 + name.len().next_multiple_of(2);
            }
        }

        let files = windows_iso_files();
        iso_directory(
            &mut bytes,
            20,
            20,
            &[
                iso_record(b"EFI", 21, SECTOR, true),
                iso_record(b"sources", 23, SECTOR, true),
                iso_record(b"SETUP.EXE;1", 24, files[0].1.len(), false),
            ],
        );
        iso_directory(&mut bytes, 21, 20, &[iso_record(b"BOOT", 22, SECTOR, true)]);
        iso_directory(
            &mut bytes,
            22,
            21,
            &[iso_record(b"BOOTX64.EFI;1", 29, files[3].1.len(), false)],
        );
        iso_directory(
            &mut bytes,
            23,
            20,
            &[
                iso_record(b"BOOT.WIM;1", 25, files[1].1.len(), false),
                iso_record(b"INSTALL.ESD;1", 28, files[2].1.len(), false),
            ],
        );
        for ((_, content), sector) in files.iter().zip([24, 25, 28, 29]) {
            bytes[sector * SECTOR..sector * SECTOR + content.len()].copy_from_slice(content);
        }
        bytes
    }

    fn open_fixture(bytes: &[u8]) -> (TempDir, crate::model::IsoImage) {
        let directory = tempdir().unwrap();
        let path = directory.path().join("windows.iso");
        fs::write(&path, bytes).unwrap();
        let image = crate::model::IsoImage::open(path).unwrap();
        (directory, image)
    }

    #[test]
    fn stages_real_iso_with_normalized_paths_contents_and_pe_architecture() {
        let bytes = windows_iso();
        let before = Sha256::digest(&bytes);
        let (directory, image) = open_fixture(&bytes);
        let target = directory.path().join("untouched-target.img");
        fs::write(&target, b"target sentinel").unwrap();
        let progress = RefCell::new(Vec::new());
        let staged = stage(&image, &AtomicBool::new(false), |copied, total| {
            progress.borrow_mut().push((copied, total));
        })
        .unwrap();

        let expected = windows_iso_files();
        let total = expected.iter().map(|(_, b)| b.len() as u64).sum();
        assert_eq!(staged.architecture, "x86_64");
        assert_eq!(staged.files.len(), expected.len());
        assert_eq!(staged.total_bytes, total);
        for (path, content) in expected {
            let file = staged
                .files
                .iter()
                .find(|f| f.relative_path == path)
                .unwrap();
            assert_eq!(file.source, staged.root.join(path));
            assert_eq!(file.size_bytes, content.len() as u64);
            let extracted = fs::read(&file.source).unwrap();
            assert_eq!(extracted, content);
            assert_eq!(Sha256::digest(&extracted), Sha256::digest(&content));
        }
        let progress = progress.into_inner();
        assert!(progress.len() >= 5);
        assert!(
            progress
                .iter()
                .all(|&(copied, t)| t == total && copied <= t)
        );
        assert!(progress.windows(2).all(|p| p[0].0 <= p[1].0));
        assert_eq!(progress.last(), Some(&(total, total)));
        assert_eq!(Sha256::digest(fs::read(&image.path).unwrap()), before);
        assert_eq!(fs::read(&target).unwrap(), b"target sentinel");
        let root = staged.root.clone();
        drop(staged);
        assert!(!root.exists(), "staging directory survived its owner");
    }

    #[test]
    fn real_iso_cancellation_before_and_during_extraction_returns_no_stage() {
        for cancelled_before_start in [true, false] {
            let bytes = windows_iso();
            let (_directory, image) = open_fixture(&bytes);
            let cancel = AtomicBool::new(cancelled_before_start);
            let progress = RefCell::new(Vec::new());
            let error = stage(&image, &cancel, |copied, total| {
                progress.borrow_mut().push((copied, total));
                cancel.store(true, Ordering::Relaxed);
            })
            .err()
            .expect("cancelled staging succeeded");
            assert!(error.contains("Staging cancelled"), "{error}");
            let progress = progress.into_inner();
            if cancelled_before_start {
                assert!(progress.is_empty());
            } else {
                assert!(!progress.is_empty());
                assert!(progress.iter().all(|&(copied, total)| copied < total));
            }
            assert_eq!(fs::read(&image.path).unwrap(), bytes);
        }
    }

    #[test]
    fn real_linux_iso_and_missing_windows_boot_file_fail_before_extraction() {
        for linux in [true, false] {
            let mut bytes = windows_iso();
            if linux {
                iso_directory(
                    &mut bytes,
                    20,
                    20,
                    &[iso_record(b"VMLINUZ;1", 24, 10, false)],
                );
            } else {
                iso_directory(&mut bytes, 22, 21, &[]);
            }
            let (_directory, image) = open_fixture(&bytes);
            let error = stage(&image, &AtomicBool::new(false), |_, _| {
                panic!("invalid installer reached extraction");
            })
            .err()
            .expect("invalid installer staged successfully");
            let expected = if linux {
                "Only standard Windows installer images"
            } else {
                "no supported UEFI boot file"
            };
            assert!(error.contains(expected), "{error}");
            assert_eq!(fs::read(&image.path).unwrap(), bytes);
        }
    }

    #[test]
    fn real_iso_rejects_out_of_bounds_extent_before_extraction() {
        let mut bytes = windows_iso();
        both_endian_u32(&mut bytes, 22 * SECTOR + 68 + 2, 64);
        let (_directory, image) = open_fixture(&bytes);
        let error = stage(&image, &AtomicBool::new(false), |_, _| {
            panic!("out-of-bounds image reached extraction");
        })
        .err()
        .expect("out-of-bounds image staged successfully");
        assert!(error.contains("bounded ISO9660/UDF"), "{error}");
        assert!(error.contains("extent exceeds image bounds"), "{error}");
        assert_eq!(fs::read(&image.path).unwrap(), bytes);
    }

    #[test]
    fn real_iso_rejects_invalid_dos_pe_offset_signature_and_machine() {
        for (offset, replacement) in [(0, 0), (60, 65), (64, b'X'), (68, 0x4c)] {
            let mut bytes = windows_iso();
            bytes[29 * SECTOR + offset] = replacement;
            let (_directory, image) = open_fixture(&bytes);
            let progress = RefCell::new(Vec::new());
            let error = stage(&image, &AtomicBool::new(false), |copied, total| {
                progress.borrow_mut().push((copied, total));
            })
            .err()
            .expect("invalid EFI loader staged successfully");
            assert!(error.contains("EFI/BOOT/BOOTX64.EFI"), "{error}");
            assert!(error.contains("PE"), "{error}");
            let progress = progress.into_inner();
            let &(copied, total) = progress.last().expect("loader was not extracted");
            assert_eq!(copied, total);
            assert_eq!(fs::read(&image.path).unwrap(), bytes);
        }
    }

    fn file(name: &str, size: u64) -> TreeNode {
        TreeNode::new_file_with_location(name.into(), size, 0, size)
    }

    #[test]
    fn rejects_windows_unsafe_names() {
        for name in [
            "",
            ".",
            "..",
            "/etc",
            "a/b",
            "a\\b",
            "C:a",
            "a\0b",
            "NUL.txt",
            "COM1",
            "LPT².txt",
            "a.",
            "a ",
            "a?",
            "a*",
            "a<",
            "a>",
            "a|",
            "a\"",
            "a\u{1f}",
        ] {
            assert!(validate_name(name).is_err(), "accepted {name:?}");
        }
        assert!(validate_name(&"🦀".repeat(128)).is_err());
        assert!(validate_name(&"a".repeat(255)).is_ok());
        assert!(validate_name("bootmgr.efi").is_ok());
    }

    #[test]
    fn sibling_collisions_are_rejected_but_separate_directories_are_not() {
        let mut root = TreeNode::new_directory("/".into());
        for name in ["one", "two"] {
            let mut dir = TreeNode::new_directory(name.into());
            dir.add_child(file("same.txt", 1));
            root.add_child(dir);
        }
        assert_eq!(validate_tree(&root, 10).unwrap(), 2);
        root.children[0].add_child(file("SAME.TXT", 1));
        assert!(validate_tree(&root, 10).is_err());
    }

    #[test]
    fn rejects_out_of_bounds_extents_and_oversized_non_wim_files() {
        let mut root = TreeNode::new_directory("/".into());
        root.add_child(file("huge", 11));
        assert!(validate_tree(&root, 10).is_err());
        root.children[0] = file("boot.wim", U32_MAX + 1);
        assert!(validate_tree(&root, U32_MAX + 10).is_err());
        root.children[0].file_location = Some(u64::MAX);
        assert!(validate_tree(&root, u64::MAX).is_err());
    }

    #[test]
    fn linux_tree_is_not_a_windows_installer() {
        let mut root = TreeNode::new_directory("/".into());
        root.add_child(file("vmlinuz", 10));
        assert!(validate_windows_tree(&root).is_err());
    }

    #[test]
    fn extraction_preserves_names_and_reports_streamed_bytes() {
        let dir = tempdir().unwrap();
        let mut root = TreeNode::new_directory("/".into());
        root.add_child(file("MixedCase.txt", 5));
        let mut input = io::Cursor::new(b"hello");
        let mut copied = 0;
        let mut files = Vec::new();
        extract_tree(
            &mut input,
            &root,
            Path::new(""),
            &mut ExtractionContext {
                root: dir.path(),
                total: 5,
                cancel: &AtomicBool::new(false),
                progress: &|_, _| {},
                copied: &mut copied,
                files: &mut files,
            },
        )
        .unwrap();
        assert_eq!(copied, 5);
        assert_eq!(files[0].relative_path, "MixedCase.txt");
        assert_eq!(fs::read(&files[0].source).unwrap(), b"hello");
    }

    #[test]
    fn cancellation_is_not_reported_as_a_successful_copy() {
        let dir = tempdir().unwrap();
        let mut output = File::create(dir.path().join("out")).unwrap();
        let cancel = AtomicBool::new(true);
        let mut copied = 0;
        let mut writer = CopyProgress {
            out: &mut output,
            copied: &mut copied,
            total: 5,
            cancel: &cancel,
            progress: &|_, _| {},
        };
        assert!(writer.write_all(b"hello").is_err());
        assert_eq!(copied, 0);
    }
}
