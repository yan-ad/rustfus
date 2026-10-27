use std::io::{Read, Seek, SeekFrom, Write};

const MIB: u64 = 1024 * 1024;
const GPT_ENTRIES_BYTES: u64 = 128 * 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutScheme {
    Mbr,
    Gpt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub start_lba: u64,
    pub sectors: u64,
    pub sector_size: u32,
}

impl Layout {
    pub fn start_bytes(&self) -> Result<u64, String> {
        self.start_lba
            .checked_mul(u64::from(self.sector_size))
            .ok_or_else(|| "Partition offset overflow".into())
    }

    pub fn length_bytes(&self) -> Result<u64, String> {
        self.sectors
            .checked_mul(u64::from(self.sector_size))
            .ok_or_else(|| "Partition length overflow".into())
    }
}

/// Plans one 1 MiB-aligned partition; any partial trailing sector is rejected.
/// MBR is limited to a 32-bit LBA address space, including the partition end.
pub fn plan(total_bytes: u64, sector_size: u32, scheme: LayoutScheme) -> Result<Layout, String> {
    if !matches!(sector_size, 512 | 4096) {
        return Err("Logical sector size must be 512 or 4096 bytes".into());
    }
    let ss = u64::from(sector_size);
    if !total_bytes.is_multiple_of(ss) {
        return Err("Disk size is not a whole number of logical sectors".into());
    }
    let total_sectors = total_bytes / ss;
    let start_lba = MIB / ss;
    let reserve = match scheme {
        LayoutScheme::Mbr => {
            if total_sectors > u64::from(u32::MAX) + 1 {
                return Err("Disk exceeds the MBR 32-bit LBA limit; use GPT".into());
            }
            0
        }
        LayoutScheme::Gpt => 1 + GPT_ENTRIES_BYTES.div_ceil(ss),
    };
    let sectors = total_sectors
        .checked_sub(start_lba + reserve)
        .filter(|n| *n > 0)
        .ok_or("Disk is too small for an aligned partition")?;
    Ok(Layout {
        start_lba,
        sectors,
        sector_size,
    })
}

/// Erases the first and last MiB, then writes a single partition table.
/// This is a quick format, not a secure erase. The caller owns confirmation,
/// exclusive access, cancellation, and any durable device sync after flush.
/// The supplied size must equal the seekable image/device length.
pub fn write_partition_table<W: Read + Write + Seek>(
    disk: &mut W,
    total_bytes: u64,
    layout: &Layout,
    scheme: LayoutScheme,
) -> Result<(), String> {
    write_partition_table_with_efi(disk, total_bytes, layout, scheme, false)
}

/// GPT uses EFI System Partition type when requested; MBR remains inactive FAT32
/// LBA (0x0c). No BIOS loader is installed.
pub fn write_partition_table_with_efi<W: Read + Write + Seek>(
    disk: &mut W,
    total_bytes: u64,
    layout: &Layout,
    scheme: LayoutScheme,
    efi: bool,
) -> Result<(), String> {
    if *layout != plan(total_bytes, layout.sector_size, scheme)? {
        return Err("Layout does not match the disk size and partition scheme".into());
    }
    let actual = disk.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    if actual != total_bytes {
        return Err("Disk length changed or does not match the planned size".into());
    }
    // Generate identities before the first destructive write.
    let disk_guid = uuid::Uuid::new_v4();
    let partition_guid = uuid::Uuid::new_v4();
    let result = (|| -> std::io::Result<()> {
        zero_range(disk, 0, total_bytes.min(MIB))?;
        zero_range(disk, total_bytes.saturating_sub(MIB), total_bytes.min(MIB))?;
        let mut mbr = vec![0u8; layout.sector_size as usize];
        mbr[440..444].copy_from_slice(&disk_guid.as_bytes()[..4]);
        let entry = &mut mbr[446..462];
        entry[1..4].copy_from_slice(&[0xfe, 0xff, 0xff]);
        entry[5..8].copy_from_slice(&[0xfe, 0xff, 0xff]);
        let (kind, start, count) = match scheme {
            LayoutScheme::Mbr => (0x0c, layout.start_lba as u32, layout.sectors as u32),
            LayoutScheme::Gpt => (
                0xee,
                1,
                (total_bytes / u64::from(layout.sector_size) - 1).min(u64::from(u32::MAX)) as u32,
            ),
        };
        entry[4] = kind;
        entry[8..12].copy_from_slice(&start.to_le_bytes());
        entry[12..16].copy_from_slice(&count.to_le_bytes());
        mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
        write_at(disk, 0, &mbr)?;
        if scheme == LayoutScheme::Gpt {
            let ss = u64::from(layout.sector_size);
            let last_lba = total_bytes / ss - 1;
            let entry_sectors = GPT_ENTRIES_BYTES.div_ceil(ss);
            let mut entries = vec![0u8; GPT_ENTRIES_BYTES as usize];
            // Microsoft basic data GUID, in GPT mixed-endian wire order.
            entries[..16].copy_from_slice(&[
                0xa2, 0xa0, 0xd0, 0xeb, 0xe5, 0xb9, 0x33, 0x44, 0x87, 0xc0, 0x68, 0xb6, 0xb7, 0x26,
                0x99, 0xc7,
            ]);
            if efi {
                entries[..16].copy_from_slice(
                    &uuid::Uuid::from_u128(0xc12a7328_f81f_11d2_ba4b_00a0c93ec93b).to_bytes_le(),
                );
            }
            entries[16..32].copy_from_slice(&partition_guid.to_bytes_le());
            entries[32..40].copy_from_slice(&layout.start_lba.to_le_bytes());
            entries[40..48].copy_from_slice(&(layout.start_lba + layout.sectors - 1).to_le_bytes());
            for (i, c) in "RustFus".encode_utf16().enumerate() {
                entries[56 + i * 2..58 + i * 2].copy_from_slice(&c.to_le_bytes());
            }
            let entries_crc = crc32fast::hash(&entries);
            let header = |current: u64, backup: u64, entries_lba: u64| {
                let mut data = vec![0u8; ss as usize];
                data[..8].copy_from_slice(b"EFI PART");
                data[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
                data[12..16].copy_from_slice(&92u32.to_le_bytes());
                data[24..32].copy_from_slice(&current.to_le_bytes());
                data[32..40].copy_from_slice(&backup.to_le_bytes());
                data[40..48].copy_from_slice(&(2 + entry_sectors).to_le_bytes());
                data[48..56].copy_from_slice(&(last_lba - entry_sectors - 1).to_le_bytes());
                data[56..72].copy_from_slice(&disk_guid.to_bytes_le());
                data[72..80].copy_from_slice(&entries_lba.to_le_bytes());
                data[80..84].copy_from_slice(&128u32.to_le_bytes());
                data[84..88].copy_from_slice(&128u32.to_le_bytes());
                data[88..92].copy_from_slice(&entries_crc.to_le_bytes());
                let crc = crc32fast::hash(&data[..92]);
                data[16..20].copy_from_slice(&crc.to_le_bytes());
                data
            };
            write_at(disk, 2 * ss, &entries)?;
            write_at(disk, (last_lba - entry_sectors) * ss, &entries)?;
            write_at(
                disk,
                last_lba * ss,
                &header(last_lba, 1, last_lba - entry_sectors),
            )?;
            write_at(disk, ss, &header(1, last_lba, 2))?;
        }
        disk.flush()
    })();
    result.map_err(|e| format!("Writing partition table: {e}"))
}

fn write_at<W: Write + Seek>(disk: &mut W, offset: u64, data: &[u8]) -> std::io::Result<()> {
    disk.seek(SeekFrom::Start(offset))?;
    disk.write_all(data)
}

fn zero_range<W: Write + Seek>(disk: &mut W, start: u64, length: u64) -> std::io::Result<()> {
    disk.seek(SeekFrom::Start(start))?;
    let zeros = [0u8; 64 * 1024];
    let mut remaining = length;
    while remaining > 0 {
        let count = remaining.min(zeros.len() as u64) as usize;
        disk.write_all(&zeros[..count])?;
        remaining -= count as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }
    fn u64_at(bytes: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
    }
    // Bitwise reference implementation, independent of the production CRC library.
    fn crc(bytes: &[u8]) -> u32 {
        let mut n = !0u32;
        for b in bytes {
            n ^= u32::from(*b);
            for _ in 0..8 {
                n = (n >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(n & 1));
            }
        }
        !n
    }

    #[test]
    fn efi_partition_types_and_backup_entries() {
        for ss in [512, 4096] {
            for efi in [false, true] {
                for scheme in [LayoutScheme::Mbr, LayoutScheme::Gpt] {
                    let size = 2 * MIB;
                    let p = plan(size, ss, scheme).unwrap();
                    let mut disk = std::io::Cursor::new(vec![0xaa; size as usize]);
                    write_partition_table_with_efi(&mut disk, size, &p, scheme, efi).unwrap();
                    let bytes = disk.get_ref();
                    assert_eq!(bytes[446], 0);
                    assert!(bytes[..440].iter().all(|b| *b == 0));
                    if scheme == LayoutScheme::Gpt {
                        let expected = if efi {
                            "c12a7328-f81f-11d2-ba4b-00a0c93ec93b"
                        } else {
                            "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7"
                        };
                        for header in [ss as usize, size as usize - ss as usize] {
                            let entries = u64_at(bytes, header + 72) as usize * ss as usize;
                            assert_eq!(
                                uuid::Uuid::from_bytes_le(
                                    bytes[entries..entries + 16].try_into().unwrap()
                                )
                                .to_string(),
                                expected
                            );
                            assert_eq!(
                                crc(&bytes[entries..entries + GPT_ENTRIES_BYTES as usize]),
                                u32_at(bytes, header + 88)
                            );
                        }
                    } else {
                        assert_eq!(bytes[450], 0x0c);
                    }
                }
            }
        }
    }

    #[test]
    fn alignment_and_limits() {
        for ss in [512, 4096] {
            for scheme in [LayoutScheme::Mbr, LayoutScheme::Gpt] {
                let p = plan(128 * MIB, ss, scheme).unwrap();
                assert_eq!(p.start_bytes().unwrap(), MIB);
                assert!(p.start_bytes().unwrap() + p.length_bytes().unwrap() <= 128 * MIB);
            }
            let limit = (u64::from(u32::MAX) + 1) * u64::from(ss);
            assert!(plan(limit, ss, LayoutScheme::Mbr).is_ok());
            assert!(plan(limit + u64::from(ss), ss, LayoutScheme::Mbr).is_err());
        }
        assert!(plan(128 * MIB + 1, 512, LayoutScheme::Gpt).is_err());
        assert!(plan(128 * MIB, 1024, LayoutScheme::Mbr).is_err());
        assert!(plan(MIB, 512, LayoutScheme::Gpt).is_err());
    }

    #[test]
    fn sparse_images_have_valid_mbr_and_both_gpt_copies() {
        for ss in [512, 4096] {
            for scheme in [LayoutScheme::Mbr, LayoutScheme::Gpt] {
                let mut image = tempfile::tempfile().unwrap();
                let size = 128 * MIB;
                image.set_len(size).unwrap();
                write_at(&mut image, 4 * MIB, b"untouched").unwrap();
                write_at(&mut image, size - 512, &[0xaa; 512]).unwrap();
                let p = plan(size, ss, scheme).unwrap();
                write_partition_table(&mut image, size, &p, scheme).unwrap();
                let mut mbr = [0; 512];
                image.seek(SeekFrom::Start(0)).unwrap();
                image.read_exact(&mut mbr).unwrap();
                assert_eq!(&mbr[510..], &[0x55, 0xaa]);
                assert_eq!(
                    mbr[450],
                    if scheme == LayoutScheme::Mbr {
                        0x0c
                    } else {
                        0xee
                    }
                );
                if scheme == LayoutScheme::Mbr {
                    assert_eq!(u32_at(&mbr, 454) as u64, p.start_lba);
                    assert_eq!(u32_at(&mbr, 458) as u64, p.sectors);
                } else {
                    assert_eq!(u32_at(&mbr, 454), 1);
                    assert_eq!(u32_at(&mbr, 458) as u64, size / u64::from(ss) - 1);
                    let last = size / u64::from(ss) - 1;
                    let mut copies = Vec::new();
                    for (current, backup) in [(1, last), (last, 1)] {
                        image
                            .seek(SeekFrom::Start(current * u64::from(ss)))
                            .unwrap();
                        let mut h = vec![0; ss as usize];
                        image.read_exact(&mut h).unwrap();
                        assert_eq!(&h[..8], b"EFI PART");
                        assert_eq!(u64_at(&h, 24), current);
                        assert_eq!(u64_at(&h, 32), backup);
                        let expected_crc = u32_at(&h, 16);
                        h[16..20].fill(0);
                        assert_eq!(crc(&h[..92]), expected_crc);
                        let mut entries = vec![0; GPT_ENTRIES_BYTES as usize];
                        image
                            .seek(SeekFrom::Start(u64_at(&h, 72) * u64::from(ss)))
                            .unwrap();
                        image.read_exact(&mut entries).unwrap();
                        assert_eq!(crc(&entries), u32_at(&h, 88));
                        assert_eq!(u64_at(&entries, 32), p.start_lba);
                        assert_eq!(u64_at(&entries, 40), p.start_lba + p.sectors - 1);
                        assert!(u64_at(&h, 40) <= p.start_lba);
                        assert!(u64_at(&h, 48) >= p.start_lba + p.sectors - 1);
                        copies.push(entries);
                    }
                    assert_eq!(copies[0], copies[1]);
                }
                image.seek(SeekFrom::Start(4 * MIB)).unwrap();
                let mut marker = [0; 9];
                image.read_exact(&mut marker).unwrap();
                assert_eq!(&marker, b"untouched");
                assert_eq!(image.metadata().unwrap().len(), size);
            }
        }
    }

    #[test]
    fn invalid_layout_is_rejected_before_writes() {
        let mut disk = std::io::Cursor::new(vec![0xa5; 2 * MIB as usize]);
        let mut p = plan(2 * MIB, 512, LayoutScheme::Gpt).unwrap();
        p.sectors += 1;
        assert!(write_partition_table(&mut disk, 2 * MIB, &p, LayoutScheme::Gpt).is_err());
        assert!(disk.get_ref().iter().all(|b| *b == 0xa5));
        let p = plan(3 * MIB, 512, LayoutScheme::Gpt).unwrap();
        assert!(write_partition_table(&mut disk, 3 * MIB, &p, LayoutScheme::Gpt).is_err());
        assert!(disk.get_ref().iter().all(|b| *b == 0xa5));
    }
}
