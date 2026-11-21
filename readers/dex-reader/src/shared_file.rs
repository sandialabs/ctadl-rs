//! A file reader whose clones share one handle but keep their own positions.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;

/// Reads `file` with positioned reads at its own position, so clones can read one
/// file from several threads at once without a lock or a shared cursor.
#[derive(Clone)]
pub(crate) struct SharedFile {
    file: Arc<File>,
    pos: u64,
    len: u64,
}

impl SharedFile {
    /// Wraps `file`, taking its length once from its metadata.
    pub(crate) fn new(file: File) -> io::Result<Self> {
        let len = file.metadata()?.len();
        Ok(Self {
            file: Arc::new(file),
            pos: 0,
            len,
        })
    }
}

impl Read for SharedFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(&*self.file, buf, self.pos)?;
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_read(&*self.file, buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for SharedFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let (base, offset) = match pos {
            SeekFrom::Start(n) => {
                self.pos = n;
                return Ok(n);
            }
            SeekFrom::End(n) => (self.len, n),
            SeekFrom::Current(n) => (self.pos, n),
        };
        self.pos = base.checked_add_signed(offset).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek to a negative or overflowing position",
            )
        })?;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file_of(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn clones_keep_their_own_position() {
        let tmp = temp_file_of(b"abcdefghij");
        let mut a = SharedFile::new(tmp.reopen().unwrap()).unwrap();
        let mut b = a.clone();
        b.seek(SeekFrom::Start(5)).unwrap();

        let mut buf = [0u8; 2];
        for expected in [(b"ab", b"fg"), (b"cd", b"hi")] {
            a.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, expected.0);
            b.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, expected.1);
        }
    }

    #[test]
    fn seek_and_read_past_end() {
        let tmp = temp_file_of(b"abcdefghij");
        let mut f = SharedFile::new(tmp.reopen().unwrap()).unwrap();

        assert_eq!(f.seek(SeekFrom::End(-3)).unwrap(), 7);
        let mut buf = [0u8; 8];
        assert_eq!(f.read(&mut buf).unwrap(), 3);
        assert_eq!(&buf[..3], b"hij");
        assert_eq!(f.read(&mut buf).unwrap(), 0);

        assert!(f.seek(SeekFrom::Current(-11)).is_err());
        assert!(f.seek(SeekFrom::End(-11)).is_err());
    }
}
