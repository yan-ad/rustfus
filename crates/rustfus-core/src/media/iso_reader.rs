//! Bounded readers for physical-partition UDF and ISO9660/Joliet.
//!
//! TreeNode/cat_node can describe only one byte range. Fragmented, sparse,
//! embedded and continuation allocations are therefore rejected, not flattened.
use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use isomage::TreeNode;

const BLOCK: u64 = 2048;
const MAX_ENTRIES: usize = 200_000;
const MAX_DEPTH: usize = 64;
const MAX_DIR_BYTES: u64 = 128 * 1024 * 1024;
type Result<T> = std::result::Result<T, String>;

struct Reader<'a> {
    file: &'a mut File,
    size: u64,
    dir_bytes: u64,
}

impl Reader<'_> {
    fn range(&self, offset: u64, len: u64) -> Result<()> {
        if offset.checked_add(len).is_none_or(|end| end > self.size) {
            return Err("Filesystem extent exceeds image bounds.".into());
        }
        Ok(())
    }

    fn read(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.range(offset, len as u64)?;
        let mut bytes = vec![0; len];
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        self.file
            .read_exact(&mut bytes)
            .map_err(|e| format!("Truncated image: {e}"))?;
        Ok(bytes)
    }

    fn directory(&mut self, offset: u64, len: u64) -> Result<Vec<u8>> {
        self.dir_bytes = self
            .dir_bytes
            .checked_add(len)
            .ok_or("Directory size overflow.")?;
        if self.dir_bytes > MAX_DIR_BYTES {
            return Err("Filesystem directory data exceeds 128 MiB.".into());
        }
        self.read(offset, len as usize)
    }

    fn descriptor(&mut self, block: u32, expected: u16) -> Result<Vec<u8>> {
        let bytes = self.read(u64::from(block) * BLOCK, BLOCK as usize)?;
        tag(&bytes, block, expected, 16)?;
        Ok(bytes)
    }
}

/// Prefer UDF whenever a UDF recognition descriptor is present. A broken UDF
/// must never fall back to the potentially truncated ISO bridge tree.
pub(super) fn parse(file: &mut File, size: u64) -> Result<TreeNode> {
    let mut r = Reader {
        file,
        size,
        dir_bytes: 0,
    };
    for sector in 16..=32 {
        let offset = sector * BLOCK;
        if offset + BLOCK > size {
            break;
        }
        let bytes = r.read(offset, BLOCK as usize)?;
        if matches!(&bytes[1..6], b"NSR02" | b"NSR03") {
            return udf(&mut r);
        }
    }
    iso(&mut r)
}

fn u16le(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes(b[p..p + 2].try_into().unwrap())
}
fn u32le(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}
fn u64le(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}

// ECMA-167 3/7.2: CRC-ITU-T, initial value zero, polynomial x^16+x^12+x^5+1.
fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in bytes {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn tag(bytes: &[u8], location: u32, id: u16, required: usize) -> Result<()> {
    if bytes.len() < required.max(16) {
        return Err("Truncated UDF descriptor.".into());
    }
    let crc_len = usize::from(u16le(bytes, 10));
    let sum = bytes[..16]
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 4)
        .fold(0u8, |s, (_, b)| s.wrapping_add(*b));
    if u16le(bytes, 0) != id
        || !matches!(u16le(bytes, 2), 2 | 3)
        || bytes[5] != 0
        || sum != bytes[4]
        || u32le(bytes, 12) != location
        || crc_len < required.saturating_sub(16)
        || crc_len > bytes.len() - 16
        || crc16(&bytes[16..16 + crc_len]) != u16le(bytes, 8)
    {
        return Err("Invalid UDF descriptor tag, location or CRC.".into());
    }
    Ok(())
}

struct Entry {
    node: TreeNode,
    offset: u64,
    len: u64,
    identity: u64,
}

// Build in an arena, then assemble bottom-up. Neither filesystem traversal nor
// tree construction recurses over untrusted input.
fn walk<F>(r: &mut Reader<'_>, root: Entry, mut list: F) -> Result<TreeNode>
where
    F: FnMut(&mut Reader<'_>, &Entry) -> Result<Vec<Entry>>,
{
    let mut arena = vec![(root, Vec::<usize>::new())];
    let mut queue = VecDeque::from([(0, 0)]);
    let mut seen = HashSet::new();
    while let Some((index, depth)) = queue.pop_front() {
        let entry = &arena[index].0;
        if !seen.insert(entry.identity) {
            return Err("Filesystem directory cycle or alias.".into());
        }
        let children = list(r, entry)?;
        if arena.len() + children.len() > MAX_ENTRIES + 1 {
            return Err("Filesystem contains more than 200000 entries.".into());
        }
        if depth == MAX_DEPTH && !children.is_empty() {
            return Err("Filesystem nesting exceeds 64 levels.".into());
        }
        for child in children {
            let next = arena.len();
            if child.node.is_directory {
                queue.push_back((next, depth + 1));
            }
            arena[index].1.push(next);
            arena.push((child, Vec::new()));
        }
    }
    let mut built: Vec<Option<TreeNode>> = (0..arena.len()).map(|_| None).collect();
    for (index, (mut entry, children)) in arena.into_iter().enumerate().rev() {
        for child in children {
            entry.node.add_child(built[child].take().unwrap());
        }
        built[index] = Some(entry.node);
    }
    Ok(built[0].take().unwrap())
}

fn name_ok(name: &str) -> Result<()> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.chars().any(|c| c == '\0' || c == '/' || c == '\\')
    {
        return Err("Invalid filesystem filename.".into());
    }
    Ok(())
}

fn utf16(bytes: &[u8]) -> Result<String> {
    if !bytes.len().is_multiple_of(2) {
        return Err("Odd UTF-16 filename length.".into());
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|p| u16::from_be_bytes([p[0], p[1]]))
        .collect();
    String::from_utf16(&units).map_err(|_| "Invalid UTF-16 filename.".into())
}

fn both32(bytes: &[u8], p: usize) -> Result<u32> {
    let n = u32le(bytes, p);
    if n != u32::from_be_bytes(bytes[p + 4..p + 8].try_into().unwrap()) {
        return Err("ISO9660 endian copies disagree.".into());
    }
    Ok(n)
}

fn both16(bytes: &[u8], p: usize) -> Result<u16> {
    let n = u16le(bytes, p);
    if n != u16::from_be_bytes(bytes[p + 2..p + 4].try_into().unwrap()) {
        return Err("ISO9660 endian copies disagree.".into());
    }
    Ok(n)
}

fn iso_record(r: &Reader<'_>, bytes: &[u8], name: String) -> Result<Entry> {
    if bytes.len() < 34 || bytes[0] as usize != bytes.len() || 33 + bytes[32] as usize > bytes.len()
    {
        return Err("Malformed ISO9660 directory record.".into());
    }
    if bytes[1] != 0
        || bytes[25] & !(1 | 2 | 16) != 0
        || bytes[26] != 0
        || bytes[27] != 0
        || both16(bytes, 28)? != 1
    {
        return Err("Unsupported ISO9660 multi-extent, special or interleaved entry.".into());
    }
    let offset = u64::from(both32(bytes, 2)?) * BLOCK;
    let len = u64::from(both32(bytes, 10)?);
    r.range(offset, len)?;
    let directory = bytes[25] & 2 != 0;
    let node = if directory {
        TreeNode::new_directory(name)
    } else {
        TreeNode::new_file_with_location(name, len, offset, len)
    };
    Ok(Entry {
        node,
        offset,
        len,
        identity: offset,
    })
}

fn iso(r: &mut Reader<'_>) -> Result<TreeNode> {
    let mut primary = None;
    let mut joliet = None;
    let mut terminated = false;
    for sector in 16..16 + 256 {
        let b = r.read(sector * BLOCK, BLOCK as usize)?;
        if &b[1..6] != b"CD001" || b[6] != 1 {
            return Err("Invalid ISO9660 volume descriptor.".into());
        }
        match b[0] {
            1 => primary = Some(b),
            2 if matches!(&b[88..91], b"%/@" | b"%/C" | b"%/E") => joliet = Some(b),
            255 => {
                terminated = true;
                break;
            }
            _ => {}
        }
    }
    if !terminated {
        return Err("ISO9660 descriptor sequence exceeds limit.".into());
    }
    let unicode = joliet.is_some();
    let b = joliet.or(primary).ok_or("No supported ISO9660 volume.")?;
    if both16(&b, 128)? != BLOCK as u16 || both16(&b, 120)? != 1 || both16(&b, 124)? != 1 {
        return Err("Unsupported ISO9660 block size or multi-volume image.".into());
    }
    let volume_size = u64::from(both32(&b, 80)?) * BLOCK;
    r.range(0, volume_size)?;
    let end = 156 + b[156] as usize;
    if end > b.len() {
        return Err("Invalid ISO9660 root record.".into());
    }
    let root = iso_record(r, &b[156..end], "/".into())?;
    if !root.node.is_directory {
        return Err("ISO9660 root is not a directory.".into());
    }
    if root
        .offset
        .checked_add(root.len)
        .is_none_or(|end| end > volume_size)
    {
        return Err("ISO9660 root exceeds declared volume.".into());
    }
    walk(r, root, |r, entry| {
        let bytes = r.directory(entry.offset, entry.len)?;
        let mut children = Vec::new();
        let mut p = 0;
        while p < bytes.len() {
            let len = bytes[p] as usize;
            if len == 0 {
                p = (p / BLOCK as usize + 1) * BLOCK as usize;
                continue;
            }
            if len < 34 || p + len > bytes.len() || p % BLOCK as usize + len > BLOCK as usize {
                return Err("Truncated ISO9660 directory record.".into());
            }
            let b = &bytes[p..p + len];
            let n = b[32] as usize;
            if n == 0 || 33 + n > len {
                return Err("Invalid ISO9660 filename length.".into());
            }
            if !(n == 1 && matches!(b[33], 0 | 1)) {
                let mut name = if unicode {
                    utf16(&b[33..33 + n])?
                } else {
                    String::from_utf8(b[33..33 + n].to_vec())
                        .map_err(|_| "Invalid ISO9660 filename.")?
                };
                if let Some((stem, version)) = name.rsplit_once(';')
                    && !version.is_empty()
                    && version.bytes().all(|c| c.is_ascii_digit())
                {
                    name = stem.to_owned();
                }
                if !unicode && name.ends_with('.') {
                    name.pop();
                }
                name_ok(&name)?;
                let child = iso_record(r, b, name)?;
                if child
                    .offset
                    .checked_add(child.len)
                    .is_none_or(|end| end > volume_size)
                {
                    return Err("ISO9660 entry exceeds declared volume.".into());
                }
                children.push(child);
                if children.len() > MAX_ENTRIES {
                    return Err("Too many directory entries.".into());
                }
            }
            p += len;
        }
        Ok(children)
    })
}

#[derive(Clone, Copy)]
struct Partition {
    start: u32,
    blocks: u32,
}
impl Partition {
    fn offset(self, r: &Reader<'_>, block: u32, len: u64) -> Result<u64> {
        let offset = u64::from(block) * BLOCK;
        if offset
            .checked_add(len)
            .is_none_or(|end| end > u64::from(self.blocks) * BLOCK)
        {
            return Err("UDF extent exceeds partition.".into());
        }
        let absolute = u64::from(self.start) * BLOCK + offset;
        r.range(absolute, len)?;
        Ok(absolute)
    }
}

fn long_ad(bytes: &[u8], p: usize) -> Result<(u32, u32)> {
    let len = u32le(bytes, p);
    if len >> 30 != 0 || u16le(bytes, p + 8) != 0 {
        return Err("Unsupported UDF extent type or partition reference.".into());
    }
    Ok((u32le(bytes, p + 4), len))
}

fn volume(r: &mut Reader<'_>, start: u32, len: u32) -> Result<(Partition, Vec<u8>)> {
    if len == 0 || u64::from(len) % BLOCK != 0 || u64::from(len) / BLOCK > 256 {
        return Err("Invalid or excessive UDF descriptor sequence.".into());
    }
    r.range(u64::from(start) * BLOCK, u64::from(len))?;
    let mut pd = None;
    let mut lvd = None;
    let mut done = false;
    for i in 0..u64::from(len) / BLOCK {
        let block = u64::from(start) + i;
        let block = u32::try_from(block).map_err(|_| "UDF block overflow.")?;
        let b = r.read(u64::from(block) * BLOCK, BLOCK as usize)?;
        let id = u16le(&b, 0);
        tag(&b, block, id, 16)?;
        match id {
            5 => {
                tag(&b, block, 5, 196)?;
                if pd.replace(b).is_some() {
                    return Err("Multiple UDF partitions are unsupported.".into());
                }
            }
            6 => {
                tag(&b, block, 6, 446)?;
                if lvd.replace(b).is_some() {
                    return Err("Multiple UDF logical volumes are unsupported.".into());
                }
            }
            8 => {
                done = true;
                break;
            }
            1 | 4 | 7 => {}
            _ => return Err("Unsupported UDF volume descriptor or continuation.".into()),
        }
    }
    if !done {
        return Err("Unterminated UDF descriptor sequence.".into());
    }
    let pd = pd.ok_or("Missing UDF partition descriptor.")?;
    let lvd = lvd.ok_or("Missing UDF logical volume descriptor.")?;
    if !matches!(&pd[25..31], b"+NSR02" | b"+NSR03")
        || u32le(&lvd, 212) != BLOCK as u32
        || u32le(&lvd, 264) != 6
        || u32le(&lvd, 268) != 1
        || lvd[440] != 1
        || lvd[441] != 6
        || u16le(&lvd, 442) != 1
        || u16le(&lvd, 444) != u16le(&pd, 22)
    {
        return Err("Unsupported UDF partition map or logical block size.".into());
    }
    let partition = Partition {
        start: u32le(&pd, 188),
        blocks: u32le(&pd, 192),
    };
    r.range(
        u64::from(partition.start) * BLOCK,
        u64::from(partition.blocks) * BLOCK,
    )?;
    Ok((partition, lvd))
}

fn udf(r: &mut Reader<'_>) -> Result<TreeNode> {
    let last = r.size / BLOCK;
    let mut selected = None;
    let mut error = "No valid UDF anchor or descriptor sequence.".to_owned();
    for block in [Some(256), last.checked_sub(257), last.checked_sub(1)]
        .into_iter()
        .flatten()
    {
        let Ok(block) = u32::try_from(block) else {
            continue;
        };
        let anchor = match r.descriptor(block, 2).and_then(|b| {
            tag(&b, block, 2, 32)?;
            Ok(b)
        }) {
            Ok(b) => b,
            Err(e) => {
                error = e;
                continue;
            }
        };
        for p in [16, 24] {
            match volume(r, u32le(&anchor, p + 4), u32le(&anchor, p)) {
                Ok(v) => {
                    selected = Some(v);
                    break;
                }
                Err(e) => error = e,
            }
        }
        if selected.is_some() {
            break;
        }
    }
    let (partition, lvd) = selected.ok_or_else(|| format!("Unusable UDF filesystem: {error}"))?;
    let (block, len) = long_ad(&lvd, 248)?;
    if len < BLOCK as u32 {
        return Err("Truncated UDF file set extent.".into());
    }
    let offset = partition.offset(r, block, u64::from(len))?;
    let fsd = r.read(offset, BLOCK as usize)?;
    tag(&fsd, block, 256, 480)?;
    if u32le(&fsd, 448) != 0 {
        return Err("UDF file set continuation is unsupported.".into());
    }
    let (root_block, root_len) = long_ad(&fsd, 400)?;
    let root = udf_entry(r, partition, root_block, root_len, "/".into())?;
    if !root.node.is_directory {
        return Err("UDF root is not a directory.".into());
    }
    walk(r, root, |r, entry| {
        let bytes = r.directory(entry.offset, entry.len)?;
        let mut children = Vec::new();
        let mut p = 0;
        while p < bytes.len() {
            if bytes.len() - p < 38 {
                return Err("Truncated UDF file identifier.".into());
            }
            let b = &bytes[p..];
            let name_len = b[19] as usize;
            let impl_len = u16le(b, 36) as usize;
            let len = (38 + impl_len + name_len + 3) & !3;
            if len > b.len() {
                return Err("Truncated UDF file identifier name.".into());
            }
            let location = (entry.offset - u64::from(partition.start) * BLOCK + p as u64) / BLOCK;
            tag(
                &b[..len],
                u32::try_from(location).map_err(|_| "UDF block overflow.")?,
                257,
                len,
            )?;
            if b[18] & !0x1f != 0 {
                return Err("Invalid UDF file characteristics.".into());
            }
            if b[18] & (4 | 8) == 0 {
                if b[18] & 16 != 0 {
                    return Err("Unsupported UDF metadata file.".into());
                }
                let raw = &b[38 + impl_len..38 + impl_len + name_len];
                let name = match raw.split_first() {
                    Some((8, rest)) => rest.iter().map(|&c| char::from(c)).collect(),
                    Some((16, rest)) => utf16(rest)?,
                    _ => return Err("Unsupported UDF filename compression.".into()),
                };
                name_ok(&name)?;
                let (block, len) = long_ad(b, 20)?;
                let child = udf_entry(r, partition, block, len, name)?;
                if child.node.is_directory != (b[18] & 2 != 0) {
                    return Err("UDF directory flag disagrees with ICB.".into());
                }
                children.push(child);
                if children.len() > MAX_ENTRIES {
                    return Err("Too many UDF directory entries.".into());
                }
            }
            p += len;
        }
        Ok(children)
    })
}

fn udf_entry(
    r: &mut Reader<'_>,
    partition: Partition,
    block: u32,
    icb_len: u32,
    name: String,
) -> Result<Entry> {
    if icb_len < BLOCK as u32 {
        return Err("Truncated UDF ICB extent.".into());
    }
    let identity = partition.offset(r, block, u64::from(icb_len))?;
    let b = r.read(identity, BLOCK as usize)?;
    let id = u16le(&b, 0);
    let base: usize = match id {
        261 => 176,
        266 => 216,
        _ => return Err("Unsupported UDF ICB descriptor type.".into()),
    };
    tag(&b, block, id, base)?;
    if u16le(&b, 20) != 4 || !matches!(b[27], 4 | 5) {
        return Err("Unsupported UDF ICB strategy or file type.".into());
    }
    if id == 266 && u32le(&b, 152) != 0 {
        return Err("Unsupported UDF named stream directory.".into());
    }
    let flags = u16le(&b, 34);
    if flags & 0xf800 != 0 {
        return Err("Unsupported UDF transformed, versioned or stream file.".into());
    }
    let mode = flags & 7;
    let stride = match mode {
        0 => 8,
        1 => 16,
        _ => return Err("Unsupported UDF embedded or extended allocation descriptors.".into()),
    };
    let size = u64le(&b, 56);
    let ext_len = u32le(&b, base - 8) as usize;
    let ad_len = u32le(&b, base - 4) as usize;
    let start = base
        .checked_add(ext_len)
        .ok_or("UDF allocation offset overflow.")?;
    let end = start
        .checked_add(ad_len)
        .ok_or("UDF allocation length overflow.")?;
    if end > b.len() || !ad_len.is_multiple_of(stride) {
        return Err("Truncated UDF allocation descriptors.".into());
    }
    tag(&b, block, id, end)?;
    let mut offset = None;
    let mut allocated = 0u64;
    for ad in b[start..end].chunks_exact(stride) {
        let len = u32le(ad, 0);
        if len >> 30 != 0 {
            return Err("Unsupported UDF sparse or continuation extent.".into());
        }
        if mode == 1 && u16le(ad, 8) != 0 {
            return Err("Unsupported UDF allocation partition reference.".into());
        }
        if len == 0 {
            continue;
        }
        let position = partition.offset(r, u32le(ad, 4), u64::from(len))?;
        if let Some(first) = offset {
            if first + allocated != position {
                return Err("Unsupported fragmented UDF file: extents are not consecutive.".into());
            }
        } else {
            offset = Some(position);
        }
        allocated += u64::from(len);
    }
    if allocated < size || allocated.saturating_sub(size) >= BLOCK {
        return Err("UDF allocation length disagrees with file information length.".into());
    }
    let offset = offset.unwrap_or(0);
    r.range(offset, size)?;
    let directory = b[27] == 4;
    let node = if directory {
        TreeNode::new_directory(name)
    } else {
        TreeNode::new_file_with_location(name, size, offset, size)
    };
    Ok(Entry {
        node,
        offset,
        len: size,
        identity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn put16(b: &mut [u8], p: usize, v: u16) {
        b[p..p + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn put32(b: &mut [u8], p: usize, v: u32) {
        b[p..p + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn put64(b: &mut [u8], p: usize, v: u64) {
        b[p..p + 8].copy_from_slice(&v.to_le_bytes());
    }
    fn seal(b: &mut [u8], id: u16, block: u32, len: usize) {
        put16(b, 0, id);
        put16(b, 2, 2);
        put32(b, 12, block);
        put16(b, 10, (len - 16) as u16);
        put16(b, 8, crc16(&b[16..len]));
        b[4] = 0;
        b[4] = b[..16].iter().fold(0u8, |s, &v| s.wrapping_add(v));
    }
    fn sector(image: &mut [u8], n: usize) -> &mut [u8] {
        &mut image[n * 2048..(n + 1) * 2048]
    }
    fn fixture_parse(image: &[u8]) -> Result<TreeNode> {
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(image).unwrap();
        parse(&mut f, image.len() as u64)
    }
    fn ad(b: &mut [u8], p: usize, block: u32, len: u32) {
        put32(b, p, len);
        put32(b, p + 4, block);
    }
    fn fe(
        b: &mut [u8],
        block: u32,
        directory: bool,
        data: u32,
        size: u64,
        extended: bool,
        short: bool,
    ) {
        let base = if extended { 216 } else { 176 };
        put16(b, 20, 4);
        b[27] = if directory { 4 } else { 5 };
        put16(b, 34, if short { 0 } else { 1 });
        put64(b, 56, size);
        put32(b, base - 4, if short { 8 } else { 16 });
        ad(b, base, data, size as u32);
        seal(
            b,
            if extended { 266 } else { 261 },
            block,
            base + if short { 8 } else { 16 },
        );
    }
    fn udf_fixture() -> Vec<u8> {
        let mut image = vec![0; 600 * 2048];
        sector(&mut image, 16)[1..6].copy_from_slice(b"NSR02");
        let b = sector(&mut image, 256);
        ad(b, 16, 257, 3 * 2048);
        ad(b, 24, 257, 3 * 2048);
        seal(b, 2, 256, 512);
        let b = sector(&mut image, 257);
        put16(b, 22, 7);
        b[25..31].copy_from_slice(b"+NSR02");
        put32(b, 188, 300);
        put32(b, 192, 200);
        seal(b, 5, 257, 512);
        let b = sector(&mut image, 258);
        put32(b, 212, 2048);
        ad(b, 248, 0, 2048);
        put32(b, 264, 6);
        put32(b, 268, 1);
        b[440] = 1;
        b[441] = 6;
        put16(b, 442, 1);
        put16(b, 444, 7);
        seal(b, 6, 258, 446);
        seal(sector(&mut image, 259), 8, 259, 512);
        let b = sector(&mut image, 300);
        ad(b, 400, 1, 2048);
        seal(b, 256, 0, 512);
        fe(sector(&mut image, 301), 1, true, 2, 48, false, true);
        let b = sector(&mut image, 302);
        put16(b, 16, 1);
        b[19] = 9;
        ad(b, 20, 3, 2048);
        b[38] = 8;
        b[39..47].copy_from_slice(b"Boot.WIM");
        seal(b, 257, 2, 48);
        fe(sector(&mut image, 303), 3, false, 4, 4099, false, false);
        sector(&mut image, 304)[..4].copy_from_slice(b"boot");
        image
    }
    fn both(b: &mut [u8], p: usize, n: u32) {
        put32(b, p, n);
        b[p + 4..p + 8].copy_from_slice(&n.to_be_bytes());
    }
    fn both_small(b: &mut [u8], p: usize, n: u16) {
        put16(b, p, n);
        b[p + 2..p + 4].copy_from_slice(&n.to_be_bytes());
    }
    fn record(name: &[u8], directory: bool, block: u32, len: u32) -> Vec<u8> {
        let mut b = vec![0; (33 + name.len() + 1) & !1];
        b[0] = b.len() as u8;
        both(&mut b, 2, block);
        both(&mut b, 10, len);
        b[25] = if directory { 2 } else { 0 };
        both_small(&mut b, 28, 1);
        b[32] = name.len() as u8;
        b[33..33 + name.len()].copy_from_slice(name);
        b
    }
    fn iso_fixture(joliet: bool) -> Vec<u8> {
        let mut image = vec![0; 100 * 2048];
        let b = sector(&mut image, 16);
        b[0] = 1;
        b[1..6].copy_from_slice(b"CD001");
        b[6] = 1;
        both(b, 80, 100);
        both_small(b, 120, 1);
        both_small(b, 124, 1);
        both_small(b, 128, 2048);
        let root = record(&[0], true, 40, 2048);
        b[156..156 + root.len()].copy_from_slice(&root);
        if joliet {
            let pvd = sector(&mut image, 16).to_vec();
            sector(&mut image, 17).copy_from_slice(&pvd);
            let b = sector(&mut image, 17);
            b[0] = 2;
            b[88..91].copy_from_slice(b"%/E");
        }
        let b = sector(&mut image, if joliet { 18 } else { 17 });
        b[0] = 255;
        b[1..6].copy_from_slice(b"CD001");
        b[6] = 1;
        let name = if joliet {
            "MixedCase.WIM;1"
                .encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect::<Vec<_>>()
        } else {
            b"BOOT.WIM;1".to_vec()
        };
        let b = record(&name, false, 41, 4);
        sector(&mut image, 40)[..b.len()].copy_from_slice(&b);
        sector(&mut image, 41)[..4].copy_from_slice(b"boot");
        image
    }

    #[test]
    fn reads_iso_and_full_joliet_names() {
        for (joliet, name) in [(false, "BOOT.WIM"), (true, "MixedCase.WIM")] {
            let image = iso_fixture(joliet);
            let tree = fixture_parse(&image).unwrap();
            assert_eq!(tree.children[0].name, name);
            assert_eq!(tree.children[0].file_location, Some(41 * 2048));
            assert_eq!(tree.children[0].file_length, Some(4));
        }
    }
    #[test]
    fn reads_udf_and_extracts_exact_information_length() {
        let image = udf_fixture();
        let tree = fixture_parse(&image).unwrap();
        let child = &tree.children[0];
        assert_eq!(child.name, "Boot.WIM");
        assert_eq!(child.file_location, Some(304 * 2048));
        assert_eq!(child.size, 4099);
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(&image).unwrap();
        let mut copied = Vec::new();
        isomage::cat_node(&mut f, child, &mut copied).unwrap();
        assert_eq!(copied.len(), 4099);
        assert_eq!(&copied[..4], b"boot");
    }
    #[test]
    fn reads_extended_file_entries_and_short_allocations() {
        let mut image = udf_fixture();
        let b = sector(&mut image, 303);
        b.fill(0);
        fe(b, 3, false, 4, 4099, true, true);
        assert_eq!(fixture_parse(&image).unwrap().children[0].size, 4099);
    }
    #[test]
    fn collapses_only_consecutive_extents_and_rejects_continuations() {
        for (next, flags, good) in [(6, 0, true), (7, 0, false), (6, 3u32 << 30, false)] {
            let mut image = udf_fixture();
            let b = sector(&mut image, 303);
            ad(b, 176, 4, 4096);
            ad(b, 192, next, 3 | flags);
            put32(b, 172, 32);
            seal(b, 261, 3, 208);
            assert_eq!(fixture_parse(&image).is_ok(), good);
        }
    }
    #[test]
    fn rejects_truncation_crc_and_unknown_partition_maps() {
        let image = udf_fixture();
        assert!(fixture_parse(&image[..305 * 2048]).is_err());
        let mut corrupt = image.clone();
        sector(&mut corrupt, 303)[56] ^= 1;
        assert!(fixture_parse(&corrupt).unwrap_err().contains("CRC"));
        let mut corrupt = image.clone();
        let b = sector(&mut corrupt, 258);
        b[440] = 2;
        seal(b, 6, 258, 446);
        assert!(fixture_parse(&corrupt).is_err());
        let mut corrupt = image;
        let b = sector(&mut corrupt, 303);
        put64(b, 56, 6000);
        seal(b, 261, 3, 192);
        assert!(fixture_parse(&corrupt).is_err());
    }
    #[test]
    fn uses_reserve_anchor_but_never_the_iso_bridge_on_invalid_udf() {
        let mut image = udf_fixture();
        let anchor = sector(&mut image, 256).to_vec();
        sector(&mut image, 256).fill(0);
        sector(&mut image, 599).copy_from_slice(&anchor);
        seal(sector(&mut image, 599), 2, 599, 512);
        assert!(fixture_parse(&image).is_ok());
        let mut image = iso_fixture(false);
        sector(&mut image, 20)[1..6].copy_from_slice(b"NSR02");
        assert!(fixture_parse(&image).is_err());
    }

    #[test]
    fn rejects_udf_directory_cycles_and_truncated_allocation_tables() {
        let mut image = udf_fixture();
        let b = sector(&mut image, 302);
        b[18] = 2;
        ad(b, 20, 1, 2048);
        seal(b, 257, 2, 48);
        assert!(fixture_parse(&image).unwrap_err().contains("cycle"));

        let mut image = udf_fixture();
        let b = sector(&mut image, 303);
        put32(b, 172, 2048);
        seal(b, 261, 3, 192);
        assert!(fixture_parse(&image).unwrap_err().contains("allocation"));
    }
    #[test]
    fn rejects_iso_cycles_multi_extent_and_out_of_bounds() {
        for rec in [
            record(b"LOOP", true, 40, 2048),
            record(b"BAD", false, 99, 4096),
        ] {
            let mut image = iso_fixture(false);
            sector(&mut image, 40).fill(0);
            sector(&mut image, 40)[..rec.len()].copy_from_slice(&rec);
            assert!(fixture_parse(&image).is_err());
        }
        let mut image = iso_fixture(false);
        sector(&mut image, 40)[25] = 0x80;
        assert!(fixture_parse(&image).is_err());
    }
    #[test]
    fn traversal_limits_depth_and_directory_bytes() {
        let mut image = iso_fixture(false);
        for block in 20..86 {
            let rec = record(b"DEEP", true, block + 1, 2048);
            sector(&mut image, block as usize).fill(0);
            sector(&mut image, block as usize)[..rec.len()].copy_from_slice(&rec);
        }
        let rec = record(&[0], true, 20, 2048);
        sector(&mut image, 16)[156..156 + rec.len()].copy_from_slice(&rec);
        assert!(fixture_parse(&image).unwrap_err().contains("64 levels"));
        let mut f = tempfile::tempfile().unwrap();
        let mut r = Reader {
            file: &mut f,
            size: u64::MAX,
            dir_bytes: MAX_DIR_BYTES,
        };
        assert!(r.directory(0, 1).unwrap_err().contains("128 MiB"));
    }
}
