//! A byte-bounded log sink shared by the tracing logger and the lightweight IME.
//!
//! Unix writers coordinate through a stable sidecar lock, then reopen the active
//! file on every write. Keeping an open log descriptor would let another process
//! keep appending to a renamed backup. Lock contention deliberately drops the
//! record. Older binaries that append directly do not participate in this bound.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 64 * 1024;
const TRUNCATED: &[u8] = b" [log record truncated]\n";

pub(crate) struct RollingFile {
    path: PathBuf,
    lock: File,
    max_file_bytes: u64,
}

impl RollingFile {
    pub(crate) fn new(path: &Path, truncate: bool) -> io::Result<Self> {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let writer = Self {
            path: path.to_owned(),
            lock: open_private(&suffixed(path, ".lock"))?,
            max_file_bytes: MAX_FILE_BYTES,
        };
        // A second process starting while the first is writing must neither
        // block nor truncate a file without owning the rotation lock.
        if let Some(_guard) = FileLock::try_acquire(&writer.lock)? {
            let file = open_private(path)?;
            if truncate || file.metadata()?.len() > writer.max_file_bytes {
                file.set_len(0)?;
            }
        }
        Ok(writer)
    }

    fn rotate(&self) -> io::Result<()> {
        let previous = suffixed(&self.path, ".1");
        let oldest = suffixed(&self.path, ".2");
        ignore_missing(fs::remove_file(&oldest))?;
        ignore_missing(fs::rename(&previous, &oldest))?;
        ignore_missing(fs::rename(&self.path, &previous))
    }
}

impl Write for RollingFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let Some(_guard) = FileLock::try_acquire(&self.lock)? else {
            return Ok(bytes.len());
        };
        let limit = MAX_RECORD_BYTES.min(self.max_file_bytes as usize);
        let truncated = bytes.len() > limit;
        let prefix = if truncated {
            utf8_prefix(bytes, limit - TRUNCATED.len())
        } else {
            bytes.len()
        };
        let record_len = prefix + if truncated { TRUNCATED.len() } else { 0 };
        let mut file = open_private(&self.path)?;
        let mut file_len = file.metadata()?.len();
        // Migrate an oversized file from an older logger without retaining it
        // as an oversized backup. This matches the former startup cleanup.
        if file_len > self.max_file_bytes {
            file.set_len(0)?;
            file_len = 0;
        }
        if file_len + record_len as u64 > self.max_file_bytes {
            drop(file);
            self.rotate()?;
            file = open_private(&self.path)?;
        }
        file.write_all(&bytes[..prefix])?;
        if truncated {
            file.write_all(TRUNCATED)?;
        }
        // This is a lossy sink: a shortened record must not be retried.
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // Every write goes directly to a file, with no user-space buffering.
        Ok(())
    }

    fn write_fmt(&mut self, args: std::fmt::Arguments<'_>) -> io::Result<()> {
        // `writeln!` on the IME calls this directly. Bound the whole formatted
        // record, not each individual fragment written by the formatter.
        let mut record = RecordBuffer(Vec::new());
        std::fmt::write(&mut record, args).map_err(io::Error::other)?;
        self.write_all(&record.0)
    }
}

struct RecordBuffer(Vec<u8>);

impl std::fmt::Write for RecordBuffer {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        // One extra byte signals truncation to write(), without allocating an
        // unbounded String for an unusually large diagnostic field.
        let remaining = (MAX_RECORD_BYTES + 1).saturating_sub(self.0.len());
        let count = text.len().min(remaining);
        self.0.extend_from_slice(&text.as_bytes()[..count]);
        Ok(())
    }
}

fn utf8_prefix(bytes: &[u8], mut length: usize) -> usize {
    while length > 0 && bytes[length] & 0b1100_0000 == 0b1000_0000 {
        length -= 1;
    }
    length
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

fn open_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Also fix a file left behind with more permissive permissions.
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn ignore_missing(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(unix)]
struct FileLock<'a>(&'a File);

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: std::ffi::c_int, operation: std::ffi::c_int) -> std::ffi::c_int;
}

#[cfg(unix)]
impl<'a> FileLock<'a> {
    fn try_acquire(file: &'a File) -> io::Result<Option<Self>> {
        use std::os::fd::AsRawFd;
        // LOCK_EX | LOCK_NB, shared by the supported macOS and Unix targets.
        // SAFETY: the descriptor remains owned for the guard's whole lifetime.
        if unsafe { flock(file.as_raw_fd(), 2 | 4) } == 0 {
            Ok(Some(Self(file)))
        } else {
            let error = io::Error::last_os_error();
            if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(unix)]
impl Drop for FileLock<'_> {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // LOCK_UN. The guard cannot outlive the still-open sidecar descriptor.
        unsafe { flock(self.0.as_raw_fd(), 8) };
    }
}

// Fastab ships on macOS. Retain in-process serialization for the other targets
// of this shared logger; its interprocess guarantee applies to Unix writers.
#[cfg(not(unix))]
struct FileLock {
    _guard: std::sync::MutexGuard<'static, ()>,
}

#[cfg(not(unix))]
impl FileLock {
    fn try_acquire(_file: &File) -> io::Result<Option<Self>> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        match LOCK.try_lock() {
            Ok(guard) => Ok(Some(Self { _guard: guard })),
            Err(std::sync::TryLockError::Poisoned(error)) => Ok(Some(Self {
                _guard: error.into_inner(),
            })),
            Err(std::sync::TryLockError::WouldBlock) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            loop {
                let path = std::env::temp_dir().join(format!(
                    "fastab-rolling-log-{}-{}",
                    std::process::id(),
                    NEXT_ID.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create log test directory: {error}"),
                }
            }
        }

        fn path(&self) -> PathBuf {
            self.0.join("test.log")
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn alternating_writers_reopen_after_rotation_and_keep_only_two_backups() {
        let dir = Directory::new();
        let path = dir.path();
        let mut first = RollingFile::new(&path, false).unwrap();
        let mut second = RollingFile::new(&path, false).unwrap();
        first.max_file_bytes = 128;
        second.max_file_bytes = 128;
        for index in 0..4 {
            let writer = if index % 2 == 0 { &mut first } else { &mut second };
            writer.write_all(&[b'a' + index as u8; 128]).unwrap();
        }
        assert_eq!(fs::read(&path).unwrap(), [b'd'; 128]);
        assert_eq!(fs::read(suffixed(&path, ".1")).unwrap(), [b'c'; 128]);
        assert_eq!(fs::read(suffixed(&path, ".2")).unwrap(), [b'b'; 128]);
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 4); // active, two backups, stable lock
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for entry in fs::read_dir(&dir.0).unwrap() {
                assert_eq!(entry.unwrap().metadata().unwrap().permissions().mode() & 0o777, 0o600);
            }
        }
    }

    #[test]
    fn oversized_records_and_formatted_fields_are_bounded() {
        let dir = Directory::new();
        let path = dir.path();
        let mut writer = RollingFile::new(&path, false).unwrap();
        let oversized = "界".repeat(MAX_RECORD_BYTES);
        writer.write_all(oversized.as_bytes()).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert!(bytes.len() <= MAX_RECORD_BYTES);
        assert!(
            std::str::from_utf8(&bytes)
                .unwrap()
                .ends_with(std::str::from_utf8(TRUNCATED).unwrap())
        );
        writeln!(writer, "field: {oversized} trailer").unwrap();
        assert!(fs::metadata(&path).unwrap().len() <= 2 * MAX_RECORD_BYTES as u64);
        assert!(std::str::from_utf8(&fs::read(&path).unwrap()).is_ok());
    }

    #[test]
    fn legacy_oversized_file_is_not_kept_as_an_oversized_backup() {
        let dir = Directory::new();
        let path = dir.path();
        let mut writer = RollingFile::new(&path, false).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        writer.write_all(b"new record\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new record\n");
        assert!(!suffixed(&path, ".1").exists());
    }

    #[cfg(unix)]
    #[test]
    fn contended_writer_drops_without_waiting_and_recovers_after_unlock() {
        let dir = Directory::new();
        let path = dir.path();
        let first = RollingFile::new(&path, false).unwrap();
        let mut second = RollingFile::new(&path, false).unwrap();
        let guard = FileLock::try_acquire(&first.lock).unwrap().unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            second.write_all(b"drop this\n").unwrap();
            let _ = done_tx.send(());
            second
        });
        let done = done_rx.recv_timeout(std::time::Duration::from_secs(5));
        // Release before joining/asserting, even if a regression blocks on flock.
        drop(guard);
        let mut second = thread.join().unwrap();
        done.unwrap();
        assert!(fs::read(&path).unwrap().is_empty());
        second.write_all(b"keep this\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"keep this\n");
    }
}
