//! Resource-preserving splitter for standard version 1.13 WIMs.
//!
//! Independently implemented from Microsoft's October 2007 "Windows Imaging
//! File Format (WIM)", sections WIM Header, WIM File Resource, Lookup Table,
//! XML Data, and Split (Spanned) WIM (including the diagram on page 16).
//! Each part has its own header, local lookup table, and identical source XML;
//! all image metadata remains in part 1, in its original lookup-table order.
//!
//! This is a structural validator, not an image verifier: encoded XPRESS/LZX
//! bytes and their existing SHA-1 values are preserved, not authenticated or
//! decompressed. XML is checked as UTF-16LE text, not schema-validated. Metadata
//! directory trees, reference counts against those trees, and codec bitstreams
//! are not interpreted. The caller must supply a trusted, immutable extracted
//! install.wim and retain the temporary directory until all parts are consumed.
//! Solid, pipable, delta, already split, and nonstandard WIMs are rejected.

use std::collections::HashSet;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const HEADER: u64 = 208;
const ENTRY: u64 = 50;
const SIZE_MASK: u64 = 0x00ff_ffff_ffff_ffff;
const COMPRESSED: u8 = 4;
const METADATA: u8 = 2;
const SPANNED: u32 = 8;
// Bounds only index/XML memory, never file-resource size.
const MAX_LOOKUP: u64 = 64 * 1024 * 1024;
const MAX_XML: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Resource {
    size: u64,
    flags: u8,
    offset: u64,
    original: u64,
}

impl Resource {
    fn parse(b: &[u8]) -> Self {
        Self {
            size: le64(b, 0) & SIZE_MASK,
            flags: b[7],
            offset: le64(b, 8),
            original: le64(b, 16),
        }
    }

    fn encode(self, b: &mut [u8]) {
        b[..8].copy_from_slice(&(self.size | (u64::from(self.flags) << 56)).to_le_bytes());
        b[8..16].copy_from_slice(&self.offset.to_le_bytes());
        b[16..24].copy_from_slice(&self.original.to_le_bytes());
    }

    fn end(self) -> Result<u64, String> {
        self.offset
            .checked_add(self.size)
            .ok_or_else(|| "WIM resource range overflows".into())
    }

    fn is_metadata(self) -> bool {
        self.flags & METADATA != 0
    }
}

struct Entry {
    resource: Resource,
    wire: [u8; 50],
}

struct Part {
    entries: Vec<usize>,
    payload: u64,
}

struct Plan {
    header: [u8; 208],
    entries: Vec<Entry>,
    xml: Vec<u8>,
    parts: Vec<Part>,
    boot_entry: Option<usize>,
    source_metadata: Metadata,
    total: u64,
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
fn io(e: std::io::Error) -> String {
    format!("WIM I/O: {e}")
}

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("WIM split cancelled".into())
    } else {
        Ok(())
    }
}

fn read_at(file: &mut File, offset: u64, bytes: &mut [u8]) -> Result<(), String> {
    file.seek(SeekFrom::Start(offset)).map_err(io)?;
    file.read_exact(bytes).map_err(io)
}

fn check_resource(r: Resource, len: u64, compressed: bool) -> Result<(), String> {
    if r.flags & !(METADATA | COMPRESSED) != 0 {
        return Err("Unsupported WIM resource flags (free, spanned, solid, or unknown)".into());
    }
    if r.size == 0 || r.original == 0 || r.offset < HEADER || r.end()? > len {
        return Err("Invalid or out-of-bounds WIM resource".into());
    }
    if r.flags & COMPRESSED == 0 {
        if r.size != r.original {
            return Err("Uncompressed WIM resource has inconsistent sizes".into());
        }
    } else if !compressed {
        return Err("Compressed WIM resource without a header compression type".into());
    }
    Ok(())
}

// Chunk offsets are relative to the data following this table, not the resource.
// Validate framing without inspecting the codec bitstream or allocating a chunk table.
fn check_chunks(file: &mut File, r: Resource, cancel: &AtomicBool) -> Result<(), String> {
    if r.flags & COMPRESSED == 0 {
        return Ok(());
    }
    let chunks = r.original.div_ceil(32768);
    let width = if r.original > u64::from(u32::MAX) {
        8
    } else {
        4
    };
    let table = (chunks - 1)
        .checked_mul(width)
        .ok_or("WIM chunk table overflow")?;
    let data = r
        .size
        .checked_sub(table)
        .filter(|n| *n > 0)
        .ok_or("Truncated WIM chunk table")?;
    file.seek(SeekFrom::Start(r.offset)).map_err(io)?;
    let mut previous = 0;
    // Buffered reads avoid one syscall per 32 KiB chunk.
    let mut reader = std::io::BufReader::new(file);
    for index in 0..chunks {
        if index % 4096 == 0 {
            cancelled(cancel)?;
        }
        let next = if index + 1 == chunks {
            data
        } else {
            let mut b = [0; 8];
            reader.read_exact(&mut b[..width as usize]).map_err(io)?;
            u64::from_le_bytes(b)
        };
        let decoded = (r.original - index * 32768).min(32768);
        if next <= previous || next > data || next - previous > decoded {
            return Err("Invalid WIM compressed chunk boundaries".into());
        }
        previous = next;
    }
    Ok(())
}

fn unchanged(file: &File, before: &Metadata) -> Result<(), String> {
    let after = file.metadata().map_err(io)?;
    if before.len() != after.len()
        || before.modified().map_err(io)? != after.modified().map_err(io)?
    {
        return Err("Input WIM changed while being processed".into());
    }
    Ok(())
}

fn inspect(file: &mut File, max: u64, cancel: &AtomicBool) -> Result<Plan, String> {
    cancelled(cancel)?;
    // Enforce FAT32 even when the caller supplies a larger budget.
    let max = max.min(u64::from(u32::MAX));
    if max <= HEADER {
        return Err("SWM part limit is too small for a WIM header".into());
    }
    let source_metadata = file.metadata().map_err(io)?;
    if !source_metadata.is_file() {
        return Err("WIM input must be a regular file".into());
    }
    let len = source_metadata.len();
    let mut h = [0; 208];
    read_at(file, 0, &mut h)?;
    if &h[..8] != b"MSWIM\0\0\0" || le32(&h, 8) != 208 || le32(&h, 12) != 0x0001_0d00 {
        return Err("Only standard 208-byte version 1.13 MSWIM files are supported".into());
    }
    let flags = le32(&h, 16);
    // Readonly and reparse-fixup are passive attributes and can be preserved.
    if flags & !(2 | 4 | 0x80 | 0x20000 | 0x40000) != 0 {
        return Err(
            "Unsupported WIM flags (split, solid/LZMS, incomplete, delta, or unknown)".into(),
        );
    }
    let compressed = flags & 2 != 0;
    let codec = flags & 0x60000;
    if (compressed && !matches!(codec, 0x20000 | 0x40000)) || (!compressed && codec != 0) {
        return Err(
            "WIM must use no compression, XPRESS, or LZX, without ambiguous codec flags".into(),
        );
    }
    if (compressed && le32(&h, 20) != 32768) || (!compressed && !matches!(le32(&h, 20), 0 | 32768))
    {
        return Err("Only standard 32 KiB WIM compression chunks are supported".into());
    }
    if le16(&h, 40) != 1 || le16(&h, 42) != 1 {
        return Err("Already split WIMs are unsupported".into());
    }
    if h[148..].iter().any(|b| *b != 0) || h[24..40].iter().all(|b| *b == 0) {
        return Err("Nonstandard WIM reserved fields or missing archive GUID".into());
    }
    let images = le32(&h, 44);
    let boot_index = le32(&h, 120);
    if images == 0 || boot_index > images {
        return Err("Invalid WIM image or boot index".into());
    }
    let lookup = Resource::parse(&h[48..72]);
    let xml_resource = Resource::parse(&h[72..96]);
    let boot = Resource::parse(&h[96..120]);
    let integrity = Resource::parse(&h[124..148]);
    for r in [lookup, xml_resource] {
        check_resource(r, len, false)?;
        if r.flags & COMPRESSED != 0 {
            return Err("Compressed structural WIM resources are unsupported".into());
        }
    }
    if !lookup.size.is_multiple_of(ENTRY) || lookup.size > MAX_LOOKUP || xml_resource.size > MAX_XML
    {
        return Err("Malformed or excessively large WIM lookup table/XML".into());
    }
    if boot_index == 0 {
        if boot != Resource::default() {
            return Err("Boot metadata without a boot image index".into());
        }
    } else {
        check_resource(boot, len, compressed)?;
    }
    if integrity != Resource::default() {
        check_resource(integrity, len, false)?;
    }

    let mut xml = vec![0; xml_resource.size as usize];
    read_at(file, xml_resource.offset, &mut xml)?;
    if !xml.len().is_multiple_of(2) || !xml.starts_with(&[0xff, 0xfe]) {
        return Err("WIM XML must be UTF-16LE with a byte-order mark".into());
    }
    let units: Vec<_> = xml[2..].chunks_exact(2).map(|b| le16(b, 0)).collect();
    let text = String::from_utf16(&units).map_err(|_| "Invalid UTF-16 in WIM XML")?;
    let text = text.trim_end_matches('\0').trim();
    if !text.contains("<WIM>") || !text.ends_with("</WIM>") {
        return Err("Missing WIM XML root element".into());
    }

    let count = (lookup.size / ENTRY) as usize;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(count)
        .map_err(|_| "Cannot allocate WIM resource index")?;
    let mut hashes = HashSet::new();
    let mut metadata = Vec::new();
    let mut ranges = vec![
        (lookup.offset, lookup.end()?),
        (xml_resource.offset, xml_resource.end()?),
    ];
    if integrity != Resource::default() {
        ranges.push((integrity.offset, integrity.end()?));
    }
    for i in 0..count {
        cancelled(cancel)?;
        let mut wire = [0; 50];
        read_at(file, lookup.offset + i as u64 * ENTRY, &mut wire)?;
        let r = Resource::parse(&wire[..24]);
        check_resource(r, len, compressed)?;
        if le16(&wire, 24) != 1 || le32(&wire, 26) == 0 {
            return Err("Invalid WIM resource part number or zero reference count".into());
        }
        if r.is_metadata() {
            if le32(&wire, 26) != 1 {
                return Err("Image metadata reference count must be one".into());
            }
            metadata.push(i);
        }
        let hash: [u8; 20] = wire[30..].try_into().unwrap();
        if hash == [0; 20] || !hashes.insert(hash) {
            return Err("Missing or duplicate resource hash in WIM lookup table".into());
        }
        check_chunks(file, r, cancel)?;
        ranges.push((r.offset, r.end()?));
        entries.push(Entry { resource: r, wire });
    }
    if metadata.len() as u64 != u64::from(images) {
        return Err("WIM image count does not match its metadata resources".into());
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|w| w[0].1 > w[1].0) {
        return Err("Overlapping resources in input WIM".into());
    }
    let boot_entry = if boot_index == 0 {
        None
    } else {
        let i = metadata[boot_index as usize - 1];
        if entries[i].resource != boot {
            return Err("Boot metadata does not match the indexed image metadata".into());
        }
        Some(i)
    };

    let base = HEADER + xml.len() as u64;
    if base > max {
        return Err("WIM header and XML exceed the SWM part limit".into());
    }
    let mut parts = vec![Part {
        entries: metadata,
        payload: 0,
    }];
    let metadata_payload = parts[0]
        .entries
        .iter()
        .map(|&i| entries[i].resource.size)
        .sum();
    parts[0].payload = metadata_payload;
    let size = |part: &Part| base + part.payload + part.entries.len() as u64 * ENTRY;
    if size(&parts[0]) > max {
        return Err(
            "All image metadata plus header, lookup table and XML must fit in SWM part 1".into(),
        );
    }
    for (i, entry) in entries.iter().enumerate() {
        if entry.resource.is_metadata() {
            continue;
        }
        let cost = entry.resource.size + ENTRY;
        if base + cost > max {
            return Err(
                "An indivisible encoded WIM resource cannot fit within the SWM part limit".into(),
            );
        }
        let last = parts.len() - 1;
        if size(&parts[last]) + cost > max {
            if parts.len() == u16::MAX as usize {
                return Err("SWM set exceeds 65535 parts".into());
            }
            parts.push(Part {
                entries: Vec::new(),
                payload: 0,
            });
        }
        let part = parts.last_mut().unwrap();
        part.entries.push(i);
        part.payload += entry.resource.size;
    }
    let total = parts
        .iter()
        .map(size)
        .try_fold(0u64, |a, b| a.checked_add(b))
        .ok_or("SWM total size overflow")?;
    unchanged(file, &source_metadata)?;
    Ok(Plan {
        header: h,
        entries,
        xml,
        parts,
        boot_entry,
        source_metadata,
        total,
    })
}

/// Read-only format/packing preflight. No output directory or disk is touched.
/// `max_part_bytes` includes headers, lookup tables and XML; FAT32's maximum
/// file length is enforced even if a larger limit is supplied.
/// This checks structural consistency only, not decompression or SHA-1 hashes.
pub fn validate(input: &Path, max_part_bytes: u64) -> Result<(), String> {
    let mut file = File::open(input).map_err(io)?;
    inspect(&mut file, max_part_bytes, &AtomicBool::new(false)).map(|_| ())
}

// Only paths successfully created by this invocation are ever removed.
struct Outputs {
    paths: Vec<PathBuf>,
    keep: bool,
}
impl Drop for Outputs {
    fn drop(&mut self) {
        if !self.keep {
            for path in &self.paths {
                let _ = fs::remove_file(path);
            }
        }
    }
}

/// Emit install.swm, install2.swm, ... in an existing private temporary directory.
/// Existing files are never overwritten. On error/cancellation, newly created
/// parts are removed (best effort); the caller should also own directory cleanup.
/// Source GUID, XML and resource hashes are retained. Integrity tables, which
/// cover the old physical layout, are deliberately omitted. Progress counts all
/// output bytes including structural data and ends at the reported total.
pub fn split(
    input: &Path,
    output_dir: &Path,
    max_part_bytes: u64,
    cancel: &AtomicBool,
    progress: impl Fn(u64, u64),
) -> Result<Vec<PathBuf>, String> {
    let mut source = File::open(input).map_err(io)?;
    let plan = inspect(&mut source, max_part_bytes, cancel)?;
    cancelled(cancel)?;
    if !output_dir.is_dir() {
        return Err("SWM output directory must already exist".into());
    }
    let names: Vec<_> = (1..=plan.parts.len())
        .map(|n| {
            output_dir.join(if n == 1 {
                "install.swm".into()
            } else {
                format!("install{n}.swm")
            })
        })
        .collect();
    // Detect all name conflicts before writing part 1; create_new closes the race.
    for path in &names {
        match fs::symlink_metadata(path) {
            Ok(_) => return Err(format!("SWM output already exists: {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(io(e)),
        }
    }
    let mut outputs = Outputs {
        paths: Vec::new(),
        keep: false,
    };
    let mut done = 0u64;
    progress(0, plan.total);
    let mut buffer = vec![0; 1024 * 1024];
    for (number, part) in plan.parts.iter().enumerate() {
        cancelled(cancel)?;
        unchanged(&source, &plan.source_metadata)?;
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&names[number])
            .map_err(io)?;
        outputs.paths.push(names[number].clone());
        let mut header = plan.header;
        header[16..20].copy_from_slice(&(le32(&plan.header, 16) | SPANNED).to_le_bytes());
        header[40..42].copy_from_slice(&((number + 1) as u16).to_le_bytes());
        header[42..44].copy_from_slice(&(plan.parts.len() as u16).to_le_bytes());
        header[96..120].fill(0);
        if number != 0 {
            header[120..124].fill(0);
        }
        header[124..148].fill(0);
        let lookup_size = part.entries.len() as u64 * ENTRY;
        let lookup_offset = HEADER + part.payload;
        // Structural resources conventionally use the METADATA flag, not COMPRESSED.
        Resource {
            size: lookup_size,
            flags: METADATA,
            offset: lookup_offset,
            original: lookup_size,
        }
        .encode(&mut header[48..72]);
        Resource {
            size: plan.xml.len() as u64,
            flags: METADATA,
            offset: lookup_offset + lookup_size,
            original: plan.xml.len() as u64,
        }
        .encode(&mut header[72..96]);
        let mut offset = HEADER;
        let mut table = Vec::with_capacity(lookup_size as usize);
        for &i in &part.entries {
            let entry = &plan.entries[i];
            let relocated = Resource {
                offset,
                ..entry.resource
            };
            if number == 0 && plan.boot_entry == Some(i) {
                relocated.encode(&mut header[96..120]);
            }
            let mut wire = entry.wire;
            relocated.encode(&mut wire[..24]);
            wire[24..26].copy_from_slice(&((number + 1) as u16).to_le_bytes());
            table.extend_from_slice(&wire);
            offset += entry.resource.size;
        }
        out.write_all(&header).map_err(io)?;
        done += HEADER;
        progress(done, plan.total);
        for &i in &part.entries {
            let r = plan.entries[i].resource;
            source.seek(SeekFrom::Start(r.offset)).map_err(io)?;
            let mut left = r.size;
            while left > 0 {
                cancelled(cancel)?;
                let n = left.min(buffer.len() as u64) as usize;
                source.read_exact(&mut buffer[..n]).map_err(io)?;
                out.write_all(&buffer[..n]).map_err(io)?;
                done += n as u64;
                left -= n as u64;
                progress(done, plan.total);
            }
        }
        cancelled(cancel)?;
        out.write_all(&table).map_err(io)?;
        out.write_all(&plan.xml).map_err(io)?;
        out.flush().map_err(io)?;
        let actual = out.metadata().map_err(io)?.len();
        let expected = HEADER + part.payload + lookup_size + plan.xml.len() as u64;
        if actual != expected || actual > max_part_bytes.min(u64::from(u32::MAX)) {
            return Err("SWM output length does not match its packing plan".into());
        }
        // Deliver progress only after closing the output handle (important on Windows).
        drop(out);
        done += table.len() as u64 + plan.xml.len() as u64;
        progress(done, plan.total);
    }
    cancelled(cancel)?;
    unchanged(&source, &plan.source_metadata)?;
    outputs.keep = true;
    Ok(std::mem::take(&mut outputs.paths))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static SERIAL: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "rustfus-wim-test-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // Deliberately independent byte-level fixture writer: no production descriptor
    // serializer/parser is used to create or check the on-disk structure.
    fn descriptor(
        bytes: &mut [u8],
        at: usize,
        size: usize,
        flags: u8,
        offset: usize,
        original: usize,
    ) {
        bytes[at..at + 8].copy_from_slice(&(size as u64).to_le_bytes());
        bytes[at + 7] = flags;
        bytes[at + 8..at + 16].copy_from_slice(&(offset as u64).to_le_bytes());
        bytes[at + 16..at + 24].copy_from_slice(&(original as u64).to_le_bytes());
    }

    fn fixture(compressed: bool) -> Vec<u8> {
        let mut b = vec![0; 208];
        b[..8].copy_from_slice(b"MSWIM\0\0\0");
        b[8..12].copy_from_slice(&208u32.to_le_bytes());
        b[12..16].copy_from_slice(&0x10d00u32.to_le_bytes());
        b[16..20].copy_from_slice(&(if compressed { 0x40002u32 } else { 0x80u32 }).to_le_bytes());
        b[20..24].copy_from_slice(&32768u32.to_le_bytes());
        b[24..40].copy_from_slice(&[0x59; 16]);
        b[40..42].copy_from_slice(&1u16.to_le_bytes());
        b[42..44].copy_from_slice(&1u16.to_le_bytes());
        b[44..48].copy_from_slice(&2u32.to_le_bytes());
        b[120..124].copy_from_slice(&2u32.to_le_bytes());
        // Interleave data and metadata to test relocation and metadata ordering.
        let specs = [
            (130, false, 0x31),
            (32, true, 0x41),
            (140, false, 0x51),
            (48, true, 0x61),
            (150, false, 0x71),
        ];
        let mut entries = Vec::new();
        for (size, metadata, byte) in specs {
            let offset = b.len();
            b.extend(std::iter::repeat_n(byte, size));
            let flags = if metadata {
                2
            } else if compressed {
                4
            } else {
                0
            };
            let mut wire = [0; 50];
            descriptor(&mut wire, 0, size, flags, offset, size);
            wire[24..26].copy_from_slice(&1u16.to_le_bytes());
            wire[26..30].copy_from_slice(&1u32.to_le_bytes());
            // These are markers, not real SHA-1 values: tests check preservation,
            // and intentionally do not pretend to verify compressed image data.
            wire[30..].fill(byte);
            if byte == 0x61 {
                descriptor(&mut b, 96, size, flags, offset, size);
            }
            entries.extend_from_slice(&wire);
        }
        let lookup = b.len();
        b.extend_from_slice(&entries);
        descriptor(&mut b, 48, entries.len(), 2, lookup, entries.len());
        let xml: Vec<_> = std::iter::once(0xfeffu16)
            .chain("<WIM><IMAGE INDEX=\"1\"/><IMAGE INDEX=\"2\"/></WIM>".encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect();
        let xml_at = b.len();
        b.extend_from_slice(&xml);
        descriptor(&mut b, 72, xml.len(), 2, xml_at, xml.len());
        let integrity = b.len();
        b.extend_from_slice(&[0xaa; 32]);
        descriptor(&mut b, 124, 32, 2, integrity, 32);
        b
    }

    fn get64(b: &[u8], at: usize) -> u64 {
        let mut out = 0;
        for i in 0..8 {
            out |= u64::from(b[at + i]) << (i * 8);
        }
        out
    }
    fn get_size(b: &[u8], at: usize) -> usize {
        (get64(b, at) & 0x00ff_ffff_ffff_ffff) as usize
    }
    fn get16(b: &[u8], at: usize) -> usize {
        usize::from(b[at]) + usize::from(b[at + 1]) * 256
    }
    fn resource_bytes(b: &[u8], at: usize) -> &[u8] {
        let offset = get64(b, at + 8) as usize;
        &b[offset..offset + get_size(b, at)]
    }

    #[test]
    fn every_part_has_local_header_table_xml_and_preserved_resources() {
        for compressed in [false, true] {
            let temp = Temp::new();
            let input = temp.0.join("extracted-random-name.wim");
            let source = fixture(compressed);
            fs::write(&input, &source).unwrap();
            let output = temp.0.join("output");
            fs::create_dir(&output).unwrap();
            let cap = 650;
            validate(&input, cap).unwrap();
            let progress_log = std::cell::RefCell::new(Vec::new());
            let paths = split(&input, &output, cap, &AtomicBool::new(false), |n, total| {
                progress_log.borrow_mut().push((n, total));
            })
            .unwrap();
            assert!(paths.len() >= 3);
            assert_eq!(paths[0].file_name().unwrap(), "install.swm");
            assert_eq!(paths[1].file_name().unwrap(), "install2.swm");
            let original_table = resource_bytes(&source, 48);
            let original_xml = resource_bytes(&source, 72);
            let mut observed = HashSet::new();
            let mut sizes = 0u64;
            for (part, path) in paths.iter().enumerate() {
                let bytes = fs::read(path).unwrap();
                sizes += bytes.len() as u64;
                assert!(bytes.len() as u64 <= cap);
                assert_eq!(&bytes[..8], b"MSWIM\0\0\0");
                assert_eq!(get64(&bytes, 8) & 0xffff_ffff, 208);
                assert_eq!(get16(&bytes, 40), part + 1);
                assert_eq!(get16(&bytes, 42), paths.len());
                assert_eq!(&bytes[24..40], &source[24..40]);
                assert_eq!(bytes[16] & 8, 8);
                assert_eq!(&bytes[124..148], &[0; 24]);
                assert_eq!(resource_bytes(&bytes, 72), original_xml);
                let table = resource_bytes(&bytes, 48);
                assert_eq!(table.len() % 50, 0);
                let mut end = 208;
                for wire in table.chunks_exact(50) {
                    let marker = wire[30];
                    assert!(observed.insert(marker));
                    assert_eq!(get16(wire, 24), part + 1);
                    let old = original_table
                        .chunks_exact(50)
                        .find(|e| e[30] == marker)
                        .unwrap();
                    assert_eq!(&wire[..8], &old[..8]);
                    assert_eq!(&wire[16..24], &old[16..24]);
                    assert_eq!(&wire[26..], &old[26..]);
                    let start = get64(wire, 8) as usize;
                    assert_eq!(start, end);
                    end = start + get_size(wire, 0);
                    assert_eq!(
                        &bytes[start..end],
                        resource_bytes(
                            &source,
                            (get64(&source, 56) as usize)
                                + original_table
                                    .chunks_exact(50)
                                    .position(|e| e[30] == marker)
                                    .unwrap()
                                    * 50
                        )
                    );
                    if wire[7] & 2 != 0 {
                        assert_eq!(part, 0);
                    }
                }
                assert_eq!(end, get64(&bytes, 56) as usize);
                if part == 0 {
                    let meta: Vec<_> = table
                        .chunks_exact(50)
                        .filter(|e| e[7] & 2 != 0)
                        .map(|e| e[30])
                        .collect();
                    assert_eq!(meta, vec![0x41, 0x61]);
                    assert_eq!(resource_bytes(&bytes, 96), &[0x61; 48]);
                } else {
                    assert_eq!(&bytes[96..124], &[0; 28]);
                }
            }
            assert_eq!(observed.len(), 5);
            let log = progress_log.into_inner();
            assert_eq!(log[0], (0, sizes));
            assert_eq!(log.last(), Some(&(sizes, sizes)));
            assert!(log.windows(2).all(|w| w[0].0 <= w[1].0));
            assert_eq!(fs::read(&input).unwrap(), source);
        }
    }

    #[test]
    fn preflight_budget_errors_create_no_output() {
        let temp = Temp::new();
        let path = temp.0.join("input.wim");
        fs::write(&path, fixture(false)).unwrap();
        for cap in [0, 208, 500] {
            assert!(validate(&path, cap).is_err());
        }
        let out = temp.0.join("absent");
        assert!(split(&path, &out, 500, &AtomicBool::new(false), |_, _| ()).is_err());
        assert!(!out.exists());
        let mut b = fixture(false);
        let lookup = get64(&b, 56) as usize;
        // Keep the physical fixture small but use a compressed one-chunk resource
        // with a 32 KiB extent claim, so the single-resource budget fails.
        let offset = b.len();
        b.extend(std::iter::repeat_n(0x31, 32768));
        descriptor(&mut b, lookup, 32768, 0, offset, 32768);
        fs::write(&path, b).unwrap();
        assert!(validate(&path, 650).unwrap_err().contains("indivisible"));
    }

    #[test]
    fn cancellation_and_source_mutation_remove_new_parts() {
        let temp = Temp::new();
        let input = temp.0.join("input.wim");
        fs::write(&input, fixture(false)).unwrap();
        let output = temp.0.join("output");
        fs::create_dir(&output).unwrap();
        let cancel = AtomicBool::new(true);
        assert!(split(&input, &output, 650, &cancel, |_, _| ()).is_err());
        assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
        cancel.store(false, Ordering::Relaxed);
        assert!(
            split(&input, &output, 650, &cancel, |n, _| {
                if n > 208 {
                    cancel.store(true, Ordering::Relaxed);
                }
            })
            .is_err()
        );
        assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
        cancel.store(false, Ordering::Relaxed);
        assert!(
            split(&input, &output, 650, &cancel, |n, _| {
                if n == 0 {
                    OpenOptions::new()
                        .write(true)
                        .open(&input)
                        .unwrap()
                        .set_len(0)
                        .unwrap();
                }
            })
            .is_err()
        );
        assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
    }

    #[test]
    fn existing_outputs_are_untouched() {
        let temp = Temp::new();
        let input = temp.0.join("input.wim");
        fs::write(&input, fixture(false)).unwrap();
        fs::write(temp.0.join("install2.swm"), b"existing").unwrap();
        assert!(split(&input, &temp.0, 650, &AtomicBool::new(false), |_, _| ()).is_err());
        assert!(!temp.0.join("install.swm").exists());
        assert_eq!(fs::read(temp.0.join("install2.swm")).unwrap(), b"existing");
    }

    #[test]
    fn malformed_and_advanced_inputs_are_rejected() {
        let temp = Temp::new();
        let input = temp.0.join("input.wim");
        let source = fixture(false);
        let lookup = get64(&source, 56) as usize;
        let mut cases = Vec::new();
        for at in [0, 8, 12, 148] {
            let mut b = source.clone();
            b[at] ^= 1;
            cases.push(b);
        }
        for flags in [8u32, 0x80002, 0x60002, 0x10, 0x20, 0x40, 0x8000_0000] {
            let mut b = source.clone();
            b[16..20].copy_from_slice(&flags.to_le_bytes());
            cases.push(b);
        }
        for resource_flag in [1, 8, 0x10, 0x20, 0x80] {
            let mut b = source.clone();
            b[lookup + 7] = resource_flag;
            cases.push(b);
        }
        for at in [lookup + 24, lookup + 26] {
            let mut b = source.clone();
            b[at] = 0;
            cases.push(b);
        }
        let mut b = source.clone();
        b[56..64].copy_from_slice(&u64::MAX.to_le_bytes());
        cases.push(b);
        let mut b = source.clone();
        b[lookup + 8..lookup + 16].copy_from_slice(&208u64.to_le_bytes());
        b[lookup + 50 + 8..lookup + 50 + 16].copy_from_slice(&208u64.to_le_bytes());
        cases.push(b);
        let mut b = source.clone();
        b[44] = 3;
        cases.push(b);
        let mut b = source.clone();
        b[120] = 1;
        cases.push(b);
        let mut b = source.clone();
        b[lookup + 30..lookup + 50].fill(0);
        cases.push(b);
        let mut b = source.clone();
        let hash = b[lookup + 30..lookup + 50].to_vec();
        b[lookup + 80..lookup + 100].copy_from_slice(&hash);
        cases.push(b);
        for end in [0, 20, 207, source.len() - 1] {
            cases.push(source[..end].to_vec());
        }
        for (i, b) in cases.iter().enumerate() {
            fs::write(&input, b).unwrap();
            assert!(
                validate(&input, 650).is_err(),
                "malformed case {i} accepted"
            );
        }
    }

    #[test]
    fn compressed_chunk_tables_are_checked_without_decompression() {
        let temp = Temp::new();
        let input = temp.0.join("input.wim");
        let mut source = fixture(true);
        let lookup = get64(&source, 56) as usize;
        let offset = source.len();
        // Two stored chunks in a compressed resource, including the table.
        source.extend_from_slice(&32768u32.to_le_bytes());
        source.extend(std::iter::repeat_n(0x21, 32768));
        source.extend_from_slice(b"last");
        descriptor(&mut source, lookup, 32776, 4, offset, 32772);
        fs::write(&input, &source).unwrap();
        validate(&input, 34000).unwrap();
        for bad in [0u32, 32769, u32::MAX] {
            source[offset..offset + 4].copy_from_slice(&bad.to_le_bytes());
            fs::write(&input, &source).unwrap();
            assert!(validate(&input, 34000).is_err());
        }
    }
}
