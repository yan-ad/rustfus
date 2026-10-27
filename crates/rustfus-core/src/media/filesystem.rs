use std::io::{self, Read, Seek, SeekFrom, Write};

use super::layout::Layout;

/// A bounded view of an existing seekable image. Positions are relative to the
/// region; writes cannot extend it and seeks outside it fail without moving it.
/// The borrowed stream's position is unspecified once the view is dropped.
pub struct Region<'a, W> {
    disk: &'a mut W,
    start: u64,
    length: u64,
    position: u64,
}

impl<'a, W: Seek> Region<'a, W> {
    pub fn new(disk: &'a mut W, start_bytes: u64, length: u64) -> io::Result<Self> {
        let end = start_bytes
            .checked_add(length)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Region end overflow"))?;
        let available = disk.seek(SeekFrom::End(0))?;
        if end > available {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Region exceeds image length",
            ));
        }
        disk.seek(SeekFrom::Start(start_bytes))?;
        Ok(Self {
            disk,
            start: start_bytes,
            length,
            position: 0,
        })
    }
}

impl<W: Read + Seek> Read for Region<'_, W> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = (self.length - self.position).min(buffer.len() as u64) as usize;
        if count == 0 {
            return Ok(0);
        }
        let read = self.disk.read(&mut buffer[..count])?;
        self.position += read as u64;
        Ok(read)
    }
}

impl<W: Write + Seek> Write for Region<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let count = (self.length - self.position).min(buffer.len() as u64) as usize;
        if count == 0 {
            return Ok(0);
        }
        let written = self.disk.write(&buffer[..count])?;
        self.position += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.disk.flush()
    }
}

impl<W: Seek> Seek for Region<'_, W> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let next = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
            SeekFrom::End(n) => i128::from(self.length) + i128::from(n),
        };
        if next < 0 || next > i128::from(self.length) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Seek outside partition region",
            ));
        }
        let next = next as u64;
        self.disk.seek(SeekFrom::Start(self.start + next))?;
        self.position = next;
        Ok(next)
    }
}

/// ASCII FAT labels are uppercased and space-padded. Empty labels are allowed;
/// control bytes, non-ASCII, forbidden FAT characters and leading/trailing spaces
/// are rejected rather than silently truncated or replaced.
pub fn validate_label(label: &str) -> Result<[u8; 11], String> {
    if label.len() > 11 || !label.is_ascii() {
        return Err("FAT32 label must contain at most 11 ASCII characters".into());
    }
    if label.starts_with(' ') || label.ends_with(' ') {
        return Err("FAT32 label cannot start or end with a space".into());
    }
    if label
        .bytes()
        .any(|b| b < 0x20 || b == 0x7f || b"\"*+,./:;<=>?[\\]|".contains(&b))
    {
        return Err("FAT32 label contains a forbidden character".into());
    }
    let mut bytes = [b' '; 11];
    for (target, source) in bytes.iter_mut().zip(label.bytes()) {
        *target = source.to_ascii_uppercase();
    }
    Ok(bytes)
}

fn format_options(
    layout: &Layout,
    label: &str,
    cluster: Option<u32>,
) -> Result<fatfs::FormatVolumeOptions, String> {
    let label = validate_label(label)?;
    if !matches!(layout.sector_size, 512 | 4096) || layout.start_lba == 0 {
        return Err(
            "FAT32 requires a nonzero partition start and 512- or 4096-byte sectors".into(),
        );
    }
    let length = layout.length_bytes()?;
    if !(32 * 1024 * 1024..=2 * 1024 * 1024 * 1024 * 1024u64).contains(&length) {
        return Err("FAT32 partition size must be between 32 MiB and 2 TiB".into());
    }
    let sectors =
        u32::try_from(layout.sectors).map_err(|_| "FAT32 exceeds its 32-bit sector count")?;
    if let Some(bytes) = cluster
        && (!bytes.is_power_of_two() || bytes < layout.sector_size || bytes > 32 * 1024)
    {
        return Err(
            "Cluster size must be a power of two between the sector size and 32 KiB".into(),
        );
    }
    layout
        .start_bytes()?
        .checked_add(length)
        .ok_or("Partition end overflow")?;
    let mut options = fatfs::FormatVolumeOptions::new()
        .fat_type(fatfs::FatType::Fat32)
        .bytes_per_sector(layout.sector_size as u16)
        .total_sectors(sectors)
        .volume_label(label)
        .volume_id(0);
    if let Some(bytes) = cluster {
        options = options.bytes_per_cluster(bytes);
    }
    Ok(options)
}

/// Validates the exact FAT32 geometry without accessing a device. Call before
/// erasing partition tables. The dry run retains only the boot sector in memory.
pub fn validate_format(layout: &Layout, label: &str, cluster: Option<u32>) -> Result<(), String> {
    geometry(layout, label, cluster).map(|_| ())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatGeometry {
    pub cluster_size: u32,
    pub data_clusters: u32,
    pub available_bytes: u64,
}

pub fn geometry(
    layout: &Layout,
    label: &str,
    cluster: Option<u32>,
) -> Result<FormatGeometry, String> {
    let options = format_options(layout, label, cluster)?;
    let mut probe = io::Cursor::new(vec![0; layout.sector_size as usize]);
    let mut region =
        Region::new(&mut probe, 0, u64::from(layout.sector_size)).map_err(|e| e.to_string())?;
    // The one-sector bound stops formatting immediately after boot serialization.
    let _ = fatfs::format_volume(&mut region, options);
    let boot = probe.get_ref();
    if &boot[510..512] != b"\x55\xaa" || boot[17..19] != [0, 0] || boot[22..24] != [0, 0] {
        return Err("Volume and cluster size do not produce a valid FAT32 filesystem; choose smaller clusters or a larger volume".into());
    }
    let reserved = u64::from(u16::from_le_bytes(boot[14..16].try_into().unwrap()));
    let fat_sectors = u64::from(u32::from_le_bytes(boot[36..40].try_into().unwrap()));
    let data = layout
        .sectors
        .checked_sub(reserved + u64::from(boot[16]) * fat_sectors)
        .ok_or("FAT metadata exceeds partition size")?;
    let sectors_per_cluster = u64::from(boot[13]);
    if !sectors_per_cluster.is_power_of_two() || fat_sectors == 0 || boot[16] != 2 {
        return Err("Invalid FAT32 geometry".into());
    }
    let clusters = data / sectors_per_cluster;
    if !(65_525..=0x0fff_ffed).contains(&clusters) {
        return Err("FAT32 requires between 65,525 and 268,435,437 data clusters".into());
    }
    let cluster_size = (sectors_per_cluster * u64::from(layout.sector_size)) as u32;
    Ok(FormatGeometry {
        cluster_size,
        data_clusters: clusters as u32,
        // Formatting allocates one data cluster for the root directory.
        available_bytes: (clusters - 1) * u64::from(cluster_size),
    })
}

/// Quick-formats only the partition region. Free data is not erased and no BIOS
/// loader is installed; fatfs supplies its non-bootable boot-sector stub.
pub fn format_fat32<W: Read + Write + Seek>(
    disk: &mut W,
    layout: &Layout,
    label: &str,
    cluster: Option<u32>,
) -> Result<(), String> {
    validate_format(layout, label, cluster)?;
    let id = uuid::Uuid::new_v4();
    let options = format_options(layout, label, cluster)?
        .volume_id(u32::from_le_bytes(id.as_bytes()[..4].try_into().unwrap()));
    let mut region = Region::new(disk, layout.start_bytes()?, layout.length_bytes()?)
        .map_err(|e| format!("Opening FAT32 partition: {e}"))?;
    fatfs::format_volume(&mut region, options).map_err(|e| format!("Formatting FAT32: {e}"))?;
    // fatfs has no hidden-sector option. Set the BPB field in both boot copies.
    // GPT starts beyond u32::MAX cannot be expressed in this legacy BPB field.
    let hidden = u32::try_from(layout.start_lba).unwrap_or(0).to_le_bytes();
    for boot_sector in [0, 6] {
        region
            .seek(SeekFrom::Start(
                boot_sector * u64::from(layout.sector_size) + 28,
            ))
            .and_then(|_| region.write_all(&hidden))
            .map_err(|e| format!("Writing FAT32 partition offset: {e}"))?;
    }
    region.flush().map_err(|e| format!("Flushing FAT32: {e}"))
}

#[cfg(test)]
mod tests {
    use super::super::layout::{LayoutScheme, plan, write_partition_table};
    use super::*;
    use std::io::Cursor;

    struct Untouched;
    impl Read for Untouched {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("preflight read device")
        }
    }
    impl Write for Untouched {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            panic!("preflight wrote device")
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("preflight flushed device")
        }
    }
    impl Seek for Untouched {
        fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
            panic!("preflight sought device")
        }
    }

    #[test]
    fn preflight_rejects_invalid_geometry_before_any_device_access() {
        let good = plan(128 * 1024 * 1024, 512, LayoutScheme::Gpt).unwrap();
        assert!(validate_format(&good, "USB", None).is_ok());
        let mut cases = vec![
            (good, "bad/name", None),
            (good, "USB", Some(32768)),
            (good, "USB", Some(0)),
        ];
        for p in [
            Layout {
                start_lba: 0,
                ..good
            },
            Layout {
                sector_size: 1024,
                ..good
            },
            Layout { sectors: 0, ..good },
            Layout {
                start_lba: u64::MAX,
                ..good
            },
            Layout {
                sectors: u64::from(u32::MAX) + 1,
                ..good
            },
            Layout {
                sector_size: 4096,
                ..good
            },
        ] {
            cases.push((p, "USB", Some(32768)));
        }
        for (layout, label, cluster) in cases {
            assert!(validate_format(&layout, label, cluster).is_err());
            assert!(format_fat32(&mut Untouched, &layout, label, cluster).is_err());
        }
    }

    #[test]
    fn fat32_cluster_count_boundary_is_exact() {
        // fatfs uses 8 reserved sectors; two 512-sector FATs leave 65,525 clusters.
        let p = Layout {
            start_lba: 2048,
            sectors: 65_525 + 8 + 2 * 512,
            sector_size: 512,
        };
        assert!(validate_format(&p, "USB", Some(512)).is_ok());
        assert!(
            validate_format(
                &Layout {
                    sectors: p.sectors - 1,
                    ..p
                },
                "USB",
                Some(512)
            )
            .is_err()
        );
    }

    #[test]
    fn region_bounds_and_seek_overflow() {
        let mut disk = Cursor::new(vec![0xaa; 32]);
        {
            let mut r = Region::new(&mut disk, 8, 8).unwrap();
            assert!(r.seek(SeekFrom::Current(i64::MIN)).is_err());
            assert!(r.seek(SeekFrom::Start(u64::MAX)).is_err());
            assert_eq!(r.stream_position().unwrap(), 0);
            assert_eq!(r.write(&[0; 16]).unwrap(), 8);
            assert!(r.write_all(&[1]).is_err());
            assert_eq!(r.seek(SeekFrom::End(-8)).unwrap(), 0);
            let mut bytes = [0xff; 16];
            assert_eq!(r.read(&mut bytes).unwrap(), 8);
            assert_eq!(&bytes[..8], &[0; 8]);
            assert_eq!(r.read(&mut bytes).unwrap(), 0);
            assert!(r.seek(SeekFrom::End(1)).is_err());
        }
        assert_eq!(&disk.get_ref()[..8], &[0xaa; 8]);
        assert_eq!(&disk.get_ref()[16..], &[0xaa; 16]);
        assert!(Region::new(&mut disk, u64::MAX, 1).is_err());
        assert!(Region::new(&mut disk, 31, 2).is_err());
        let mut empty = Region::new(&mut disk, 32, 0).unwrap();
        assert_eq!(empty.write(&[1]).unwrap(), 0);
    }

    #[test]
    fn label_and_cluster_validation_precede_writes() {
        assert_eq!(validate_label("usb disk").unwrap(), *b"USB DISK   ");
        for invalid in [
            "twelve chars!",
            "bad/name",
            "é",
            " leading",
            "trailing ",
            "bad\n",
            "a\u{7f}",
        ] {
            assert!(validate_label(invalid).is_err(), "{invalid:?}");
        }
        let mut disk = tempfile::tempfile().unwrap();
        disk.set_len(128 * 1024 * 1024).unwrap();
        let p = plan(128 * 1024 * 1024, 512, LayoutScheme::Mbr).unwrap();
        for cluster in [0, 256, 1000, 65536, u32::MAX] {
            assert!(format_fat32(&mut disk, &p, "USB", Some(cluster)).is_err());
        }
        let mut first = [1; 512];
        disk.seek(SeekFrom::Start(p.start_bytes().unwrap()))
            .unwrap();
        disk.read_exact(&mut first).unwrap();
        assert_eq!(first, [0; 512]);
    }

    #[test]
    fn sparse_fat32_images_mount_and_roundtrip_files() {
        for (ss, size, cluster) in [
            (512, 128u64, Some(512)),
            (512, 128, None),
            (4096, 512, Some(4096)),
            (4096, 512, None),
        ] {
            for scheme in [LayoutScheme::Mbr, LayoutScheme::Gpt] {
                let size = size * 1024 * 1024;
                let mut disk = tempfile::tempfile().unwrap();
                disk.set_len(size).unwrap();
                let p = plan(size, ss, scheme).unwrap();
                write_partition_table(&mut disk, size, &p, scheme).unwrap();
                let after = p.start_bytes().unwrap() + p.length_bytes().unwrap();
                disk.seek(SeekFrom::Start(0)).unwrap();
                let mut table = vec![0; p.start_bytes().unwrap() as usize];
                disk.read_exact(&mut table).unwrap();
                disk.seek(SeekFrom::Start(after)).unwrap();
                let mut tail = vec![0; (size - after) as usize];
                disk.read_exact(&mut tail).unwrap();
                format_fat32(&mut disk, &p, "rustfus", cluster).unwrap();
                {
                    let region = Region::new(
                        &mut disk,
                        p.start_bytes().unwrap(),
                        p.length_bytes().unwrap(),
                    )
                    .unwrap();
                    let fs = fatfs::FileSystem::new(region, fatfs::FsOptions::new()).unwrap();
                    assert_eq!(fs.fat_type(), fatfs::FatType::Fat32);
                    assert_eq!(fs.volume_label(), "RUSTFUS");
                    assert_eq!(
                        fs.read_volume_label_from_root_dir().unwrap().as_deref(),
                        Some("RUSTFUS")
                    );
                    if let Some(bytes) = cluster {
                        assert_eq!(fs.cluster_size(), bytes);
                    }
                    let expected = geometry(&p, "rustfus", cluster).unwrap();
                    let stats = fs.stats().unwrap();
                    assert_eq!(fs.cluster_size(), expected.cluster_size);
                    assert_eq!(stats.total_clusters(), expected.data_clusters);
                    assert_eq!(
                        u64::from(stats.free_clusters()) * u64::from(fs.cluster_size()),
                        expected.available_bytes
                    );
                    let root = fs.root_dir();
                    assert_eq!(root.iter().count(), 0);
                    let mut file = root.create_file("hello.txt").unwrap();
                    file.write_all(b"partition bounded filesystem").unwrap();
                    file.flush().unwrap();
                    drop(file);
                    drop(root);
                    fs.unmount().unwrap();
                }
                {
                    let region = Region::new(
                        &mut disk,
                        p.start_bytes().unwrap(),
                        p.length_bytes().unwrap(),
                    )
                    .unwrap();
                    let fs = fatfs::FileSystem::new(region, fatfs::FsOptions::new()).unwrap();
                    let mut text = String::new();
                    fs.root_dir()
                        .open_file("hello.txt")
                        .unwrap()
                        .read_to_string(&mut text)
                        .unwrap();
                    assert_eq!(text, "partition bounded filesystem");
                    fs.unmount().unwrap();
                }
                disk.seek(SeekFrom::Start(0)).unwrap();
                let mut current = vec![0; table.len()];
                disk.read_exact(&mut current).unwrap();
                assert_eq!(table, current);
                disk.seek(SeekFrom::Start(after)).unwrap();
                let mut current = vec![0; tail.len()];
                disk.read_exact(&mut current).unwrap();
                assert_eq!(tail, current);
                assert_eq!(disk.metadata().unwrap().len(), size);
            }
        }
    }

    #[test]
    fn incompatible_fat32_cluster_counts_fail_without_mutating_boot() {
        let mut disk = tempfile::tempfile().unwrap();
        disk.set_len(128 * 1024 * 1024).unwrap();
        for ss in [512, 4096] {
            let p = plan(128 * 1024 * 1024, ss, LayoutScheme::Gpt).unwrap();
            assert!(format_fat32(&mut disk, &p, "USB", Some(32768)).is_err());
            if ss == 4096 {
                assert!(format_fat32(&mut disk, &p, "USB", None).is_err());
            }
            disk.seek(SeekFrom::Start(p.start_bytes().unwrap()))
                .unwrap();
            let mut boot = [1; 512];
            disk.read_exact(&mut boot).unwrap();
            assert_eq!(boot, [0; 512]);
        }
    }
}
