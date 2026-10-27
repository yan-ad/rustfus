use std::io::{self, Read, Seek, SeekFrom, Write};

/// Byte-addressed access backed exclusively by whole-sector transfers.
/// Call `flush` and handle its result before declaring success. Dropping this
/// adapter discards its last dirty sector without performing any further I/O.
#[must_use]
pub struct SectorIo<'a, T> {
    inner: &'a mut T,
    capacity: u64,
    position: u64,
    buffer: Vec<u8>,
    cached_offset: Option<u64>,
    dirty: bool,
}

impl<'a, T: Read + Write + Seek> SectorIo<'a, T> {
    const MAX_TRANSFER: usize = 1024 * 1024;

    pub fn new(inner: &'a mut T, capacity: u64, sector_size: u32) -> io::Result<Self> {
        if !matches!(sector_size, 512 | 4096)
            || capacity == 0
            || !capacity.is_multiple_of(u64::from(sector_size))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid disk sector geometry",
            ));
        }
        Ok(Self {
            inner,
            capacity,
            position: 0,
            buffer: vec![0; sector_size as usize],
            cached_offset: None,
            dirty: false,
        })
    }

    fn direct_len(&self, len: usize) -> usize {
        let sector_size = self.buffer.len();
        if !self.position.is_multiple_of(sector_size as u64) {
            return 0;
        }
        let count = (len.min(Self::MAX_TRANSFER) as u64).min(self.capacity - self.position);
        count as usize / sector_size * sector_size
    }

    fn flush_sector(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let offset = self
            .cached_offset
            .expect("dirty sectors have a cache address");
        self.inner.seek(SeekFrom::Start(offset))?;
        let count = loop {
            match self.inner.write(&self.buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => break result?,
            }
        };
        // Retrying only the tail of a short transfer would violate raw alignment.
        if count != self.buffer.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "Short raw sector write",
            ));
        }
        self.dirty = false;
        Ok(())
    }

    fn load_sector(&mut self) -> io::Result<usize> {
        let sector_size = self.buffer.len() as u64;
        let offset = self.position / sector_size * sector_size;
        if self.cached_offset != Some(offset) {
            self.flush_sector()?;
            // A failed read may overwrite part of the buffer: never retain its old tag.
            self.cached_offset = None;
            self.inner.seek(SeekFrom::Start(offset))?;
            let count = loop {
                match self.inner.read(&mut self.buffer) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    result => break result?,
                }
            };
            if count != self.buffer.len() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Short raw sector read",
                ));
            }
            self.cached_offset = Some(offset);
        }
        Ok((self.position - offset) as usize)
    }
}

impl<T: Read + Write + Seek> Read for SectorIo<'_, T> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.position == self.capacity {
            return Ok(0);
        }
        let count = self.direct_len(output.len());
        if count != 0 {
            self.flush_sector()?;
            self.cached_offset = None;
            self.inner.seek(SeekFrom::Start(self.position))?;
            let transferred = loop {
                match self.inner.read(&mut output[..count]) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    result => break result?,
                }
            };
            if transferred != count {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Short raw aligned read",
                ));
            }
            self.position += count as u64;
            return Ok(count);
        }
        let offset = self.load_sector()?;
        let count = output.len().min(self.buffer.len() - offset);
        output[..count].copy_from_slice(&self.buffer[offset..offset + count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl<T: Read + Write + Seek> Write for SectorIo<'_, T> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.is_empty() {
            return Ok(0);
        }
        if input.len() as u64 > self.capacity - self.position {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "Write exceeds disk capacity",
            ));
        }
        let count = self.direct_len(input.len());
        if count != 0 {
            self.flush_sector()?;
            self.cached_offset = None;
            self.inner.seek(SeekFrom::Start(self.position))?;
            let transferred = loop {
                match self.inner.write(&input[..count]) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    result => break result?,
                }
            };
            // Do not let write_all retry a potentially unaligned short-write tail.
            if transferred != count {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "Short raw aligned write",
                ));
            }
            self.position += count as u64;
            return Ok(count);
        }
        let offset = self.load_sector()?;
        let count = input.len().min(self.buffer.len() - offset);
        self.buffer[offset..offset + count].copy_from_slice(&input[..count]);
        self.dirty = true;
        self.position += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_sector()?;
        self.inner.flush()
    }
}

impl<T: Read + Write + Seek> Seek for SectorIo<'_, T> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.capacity) + i128::from(offset),
        };
        if position < 0 || position > i128::from(self.capacity) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Seek exceeds disk bounds",
            ));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Strict {
        data: Cursor<Vec<u8>>,
        sector: usize,
        reads: usize,
        writes: usize,
        flushes: usize,
        fail_write: bool,
        fail_flush: bool,
        short_write: bool,
        short_read: bool,
    }

    impl Strict {
        fn new(sector: usize) -> Self {
            Self {
                data: Cursor::new((0..sector * 4).map(|i| (i % 251) as u8).collect()),
                sector,
                reads: 0,
                writes: 0,
                flushes: 0,
                fail_write: false,
                fail_flush: false,
                short_write: false,
                short_read: false,
            }
        }
        fn check(&self, len: usize) {
            assert_eq!(self.data.position() % self.sector as u64, 0);
            assert_eq!(len % self.sector, 0);
            assert!(self.data.position() + len as u64 <= self.data.get_ref().len() as u64);
        }
    }

    impl Read for Strict {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.check(bytes.len());
            self.reads += 1;
            if self.short_read {
                self.short_read = false;
                let len = bytes.len() / 2;
                return self.data.read(&mut bytes[..len]);
            }
            self.data.read(bytes)
        }
    }
    impl Write for Strict {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.check(bytes.len());
            if self.fail_write {
                return Err(io::Error::other("injected write failure"));
            }
            self.writes += 1;
            if self.short_write {
                self.short_write = false;
                return self.data.write(&bytes[..bytes.len() / 2]);
            }
            self.data.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.fail_flush {
                Err(io::Error::other("injected flush failure"))
            } else {
                Ok(())
            }
        }
    }
    impl Seek for Strict {
        fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
            match from {
                SeekFrom::Start(offset) => assert_eq!(offset % self.sector as u64, 0),
                _ => panic!("raw seeks must use absolute aligned offsets"),
            }
            self.data.seek(from)
        }
    }

    #[test]
    fn aligned_megabyte_transfers_bypass_sector_cache() {
        for sector in [512, 4096] {
            let mut raw = Strict::new(sector);
            let payload = vec![0xa5; 1024 * 1024];
            raw.data.get_mut().resize(payload.len(), 0);
            let mut io = SectorIo::new(&mut raw, payload.len() as u64, sector as u32).unwrap();
            io.write_all(&payload).unwrap();
            assert_eq!(io.inner.reads, 0);
            assert_eq!(io.inner.writes, 1);
            assert!(!io.dirty);
            io.seek(SeekFrom::Start(0)).unwrap();
            let mut actual = vec![0; payload.len()];
            io.read_exact(&mut actual).unwrap();
            assert_eq!(actual, payload);
            assert_eq!(io.inner.reads, 1);
        }
    }

    #[test]
    fn direct_transfers_are_bounded_and_reads_stop_at_capacity() {
        for sector in [512, 4096] {
            let mut raw = Strict::new(sector);
            let payload = vec![0xa5; 2 * 1024 * 1024 + sector];
            raw.data.get_mut().resize(payload.len(), 0);
            let mut io = SectorIo::new(&mut raw, payload.len() as u64, sector as u32).unwrap();
            assert_eq!(io.write(&payload).unwrap(), 1024 * 1024);
            io.write_all(&payload[1024 * 1024..]).unwrap();
            assert_eq!(io.inner.writes, 3);
            io.seek(SeekFrom::Start(0)).unwrap();
            let mut actual = vec![0; payload.len() + sector];
            let mut total = 0;
            loop {
                let count = io.read(&mut actual[total..]).unwrap();
                assert!(count <= 1024 * 1024);
                if count == 0 {
                    break;
                }
                total += count;
            }
            assert_eq!(total, payload.len());
            assert_eq!(&actual[..total], payload);
            assert_eq!(io.inner.reads, 3);
        }
    }

    #[test]
    fn direct_reads_flush_dirty_cache_and_writes_invalidate_clean_cache() {
        for sector in [512, 4096] {
            let mut raw = Strict::new(sector);
            let capacity = raw.data.get_ref().len() as u64;
            let mut io = SectorIo::new(&mut raw, capacity, sector as u32).unwrap();
            io.write_all(b"pending").unwrap();
            io.seek(SeekFrom::Start(0)).unwrap();
            let mut actual = vec![0; sector];
            io.read_exact(&mut actual).unwrap();
            assert_eq!(&actual[..7], b"pending");
            assert!(!io.dirty);
            assert_eq!(io.cached_offset, None);
            io.seek(SeekFrom::Start(0)).unwrap();
            io.read_exact(&mut [0; 1]).unwrap();
            assert_eq!(io.cached_offset, Some(0));
            io.seek(SeekFrom::Start(0)).unwrap();
            io.write_all(&vec![0x5a; sector]).unwrap();
            io.seek(SeekFrom::Start(0)).unwrap();
            let mut byte = [0];
            io.read_exact(&mut byte).unwrap();
            assert_eq!(byte, [0x5a]);
        }
    }

    #[test]
    fn failed_direct_transfer_flush_preserves_pending_sector() {
        let mut raw = Strict::new(512);
        let mut io = SectorIo::new(&mut raw, 2048, 512).unwrap();
        io.write_all(b"pending").unwrap();
        io.seek(SeekFrom::Start(512)).unwrap();
        io.inner.fail_write = true;
        assert!(io.write(&[0xa5; 512]).is_err());
        assert!(io.read(&mut [0; 512]).is_err());
        assert_eq!(io.position, 512);
        assert_eq!(io.cached_offset, Some(0));
        assert!(io.dirty);
        assert_eq!(&io.buffer[..7], b"pending");
        io.inner.fail_write = false;
        io.write_all(&[0xa5; 512]).unwrap();
        assert_eq!(&io.inner.data.get_ref()[..7], b"pending");
        assert_eq!(&io.inner.data.get_ref()[512..1024], &[0xa5; 512]);
    }

    #[test]
    fn short_direct_transfers_fail_without_retrying_or_advancing() {
        for sector in [512, 4096] {
            for sectors in [1, 2] {
                let mut raw = Strict::new(sector);
                let capacity = raw.data.get_ref().len() as u64;
                let mut io = SectorIo::new(&mut raw, capacity, sector as u32).unwrap();
                let payload = vec![0xa5; sector * sectors];
                io.inner.short_write = true;
                assert_eq!(
                    io.write_all(&payload).unwrap_err().kind(),
                    io::ErrorKind::WriteZero
                );
                assert_eq!(io.position, 0);
                assert_eq!(io.cached_offset, None);
                assert_eq!(io.inner.writes, 1);
                io.write_all(&payload).unwrap();
                io.seek(SeekFrom::Start(0)).unwrap();
                io.inner.short_read = true;
                let mut actual = vec![0; payload.len()];
                assert_eq!(
                    io.read_exact(&mut actual).unwrap_err().kind(),
                    io::ErrorKind::UnexpectedEof
                );
                assert_eq!(io.position, 0);
                assert_eq!(io.cached_offset, None);
                assert_eq!(io.inner.reads, 1);
                io.read_exact(&mut actual).unwrap();
                assert_eq!(actual, payload);
            }
        }
    }

    #[test]
    fn unaligned_cross_sector_writes_preserve_surrounding_bytes() {
        for sector in [512, 4096] {
            let mut raw = Strict::new(sector);
            let mut expected = raw.data.get_ref().clone();
            let capacity = expected.len() as u64;
            let start = sector - 7;
            let payload = vec![0xa5; sector * 2 + 13];
            expected[start..start + payload.len()].copy_from_slice(&payload);
            let mut io = SectorIo::new(&mut raw, capacity, sector as u32).unwrap();
            io.seek(SeekFrom::Start(start as u64)).unwrap();
            io.write_all(&payload).unwrap();
            io.seek(SeekFrom::Start(0)).unwrap();
            let mut actual = vec![0; capacity as usize];
            io.read_exact(&mut actual).unwrap();
            assert_eq!(actual, expected);
            io.flush().unwrap();
            drop(io);
            assert_eq!(*raw.data.get_ref(), expected);
            assert!(raw.writes >= 3);
            assert_eq!(raw.flushes, 1);
        }
    }

    #[test]
    fn last_dirty_sector_requires_explicit_flush_and_drop_does_not_write() {
        let mut raw = Strict::new(512);
        let original = raw.data.get_ref().clone();
        {
            let mut io = SectorIo::new(&mut raw, 2048, 512).unwrap();
            io.write_all(b"pending").unwrap();
            assert!(io.dirty);
            assert_eq!(io.inner.writes, 0);
        }
        assert_eq!(*raw.data.get_ref(), original);
        {
            let mut io = SectorIo::new(&mut raw, 2048, 512).unwrap();
            io.write_all(b"committed").unwrap();
            io.flush().unwrap();
        }
        assert_eq!(&raw.data.get_ref()[..9], b"committed");
    }

    #[test]
    fn bounds_fail_without_touching_raw_io_or_changing_position() {
        let mut raw = Strict::new(512);
        let original = raw.data.get_ref().clone();
        let mut io = SectorIo::new(&mut raw, 1024, 512).unwrap();
        assert_eq!(io.seek(SeekFrom::End(0)).unwrap(), 1024);
        assert_eq!(io.read(&mut [0; 3]).unwrap(), 0);
        assert!(io.write(b"x").is_err());
        for from in [
            SeekFrom::Start(u64::MAX),
            SeekFrom::End(1),
            SeekFrom::Current(i64::MAX),
            SeekFrom::Current(i64::MIN),
        ] {
            assert!(io.seek(from).is_err());
            assert_eq!(io.stream_position().unwrap(), 1024);
        }
        io.seek(SeekFrom::End(-1)).unwrap();
        assert!(io.write(b"xx").is_err());
        io.write_all(b"z").unwrap();
        io.flush().unwrap();
        drop(io);
        assert_eq!(&raw.data.get_ref()[1024..], &original[1024..]);
        assert_eq!(raw.data.get_ref()[1023], b'z');
    }

    #[test]
    fn errors_keep_dirty_data_retryable_and_propagate_flush_failure() {
        let mut raw = Strict::new(512);
        let mut io = SectorIo::new(&mut raw, 2048, 512).unwrap();
        io.write_all(b"retry me").unwrap();
        io.inner.fail_write = true;
        assert!(io.flush().is_err());
        assert!(io.dirty);
        io.seek(SeekFrom::Start(512)).unwrap();
        assert!(io.write(b"next").is_err());
        assert_eq!(io.position, 512);
        assert_eq!(io.cached_offset, Some(0));
        io.inner.fail_write = false;
        io.inner.short_write = true;
        assert!(io.flush().is_err());
        assert!(io.dirty);
        io.inner.fail_flush = true;
        assert!(io.flush().is_err());
        assert!(!io.dirty);
        io.inner.fail_flush = false;
        io.flush().unwrap();
        assert_eq!(&io.inner.data.get_ref()[..8], b"retry me");
    }

    #[test]
    fn short_raw_reads_invalidate_cache_and_can_be_retried() {
        let mut raw = Strict::new(512);
        let expected = raw.data.get_ref()[512];
        let mut io = SectorIo::new(&mut raw, 2048, 512).unwrap();
        io.read_exact(&mut [0; 1]).unwrap();
        io.seek(SeekFrom::Start(512)).unwrap();
        io.inner.short_read = true;
        assert_eq!(
            io.read(&mut [0; 1]).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(io.cached_offset, None);
        let mut byte = [0];
        io.read_exact(&mut byte).unwrap();
        assert_eq!(byte[0], expected);
    }

    #[test]
    fn invalid_geometry_is_rejected_without_raw_access() {
        let mut raw = Strict::new(512);
        for (capacity, sector) in [(0, 512), (1023, 512), (4096, 0), (4096, 1024), (512, 4096)] {
            assert!(SectorIo::new(&mut raw, capacity, sector).is_err());
        }
        assert_eq!(raw.data.position(), 0);
        assert_eq!(raw.writes, 0);
    }
}
