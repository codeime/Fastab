//! A generation-aware view of one published `specs-ir` directory.
//!
//! The publisher replaces the directory at a canonical path. A lazy reader
//! must not resolve that path and blindly accept whichever generation happens
//! to be there. The snapshot records the relative tree and SHA-256 of every
//! regular file while opening the directory once. Unix lazy reads use a fresh
//! `openat` walk rooted at the canonical directory with `O_NOFOLLOW`, then
//! compare the bytes with the recorded digest. A replacement therefore either
//! reads identical content or fails closed and marks the snapshot stale; it
//! can never return a different generation's bytes.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

#[cfg(unix)]
use std::ffi::{CStr, CString};
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};

/// One opened generation of the IR directory. Cloning this value is cheap and
/// shares stale state with every Registry/JsHost clone.
#[derive(Clone, Debug)]
pub(crate) struct DirectorySnapshot {
    inner: Arc<SnapshotInner>,
}

#[derive(Debug)]
struct SnapshotInner {
    display_path: PathBuf,
    entries: HashMap<PathBuf, EntryKind>,
    file_digests: HashMap<PathBuf, String>,
    generation: Generation,
    stale: AtomicBool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Generation {
    #[cfg(unix)]
    root_identity: RootIdentity,
    #[cfg(not(unix))]
    canonical_path: PathBuf,
    marker_digest: Option<String>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RootIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    File,
    Directory,
}

impl DirectorySnapshot {
    /// Open and validate the directory shape while recording a digest for
    /// every regular file. No per-file descriptor survives this call.
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let root_fd = open_root(path)?;
            let root_stat = fstat_fd(root_fd.as_raw_fd())?;
            let mut entries = HashMap::new();
            entries.insert(PathBuf::new(), EntryKind::Directory);
            let mut file_digests = HashMap::new();
            capture_tree(root_fd.as_raw_fd(), Path::new(""), &mut entries, &mut file_digests)?;
            let marker_digest = file_digests.get(Path::new(".spec-pair.json")).cloned();
            Ok(Self {
                inner: Arc::new(SnapshotInner {
                    display_path: path.to_path_buf(),
                    entries,
                    file_digests,
                    generation: Generation {
                        root_identity: root_identity(&root_stat),
                        marker_digest,
                    },
                    stale: AtomicBool::new(false),
                }),
            })
        }

        #[cfg(not(unix))]
        {
            let metadata = std::fs::symlink_metadata(path)?;
            if !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!("{} is not a directory", path.display()),
                ));
            }
            let canonical_path = std::fs::canonicalize(path)?;
            let mut entries = HashMap::new();
            entries.insert(PathBuf::new(), EntryKind::Directory);
            let mut file_digests = HashMap::new();
            capture_tree_path(path, Path::new(""), &mut entries, &mut file_digests)?;
            let marker_digest = file_digests.get(Path::new(".spec-pair.json")).cloned();
            Ok(Self {
                inner: Arc::new(SnapshotInner {
                    display_path: path.to_path_buf(),
                    entries,
                    file_digests,
                    generation: Generation {
                        canonical_path,
                        marker_digest,
                    },
                    stale: AtomicBool::new(false),
                }),
            })
        }
    }

    pub(crate) fn display_path(&self) -> &Path {
        &self.inner.display_path
    }

    /// Read a regular file from the canonical directory and verify that it is
    /// still the file recorded when this generation was opened.
    #[allow(clippy::verbose_file_reads)]
    pub(crate) fn read_file(&self, relative: &Path) -> io::Result<Vec<u8>> {
        validate_relative_path(relative)?;
        let Some(expected) = self.inner.file_digests.get(relative) else {
            self.mark_stale();
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("snapshot file not found: {}", relative.display()),
            ));
        };

        #[cfg(unix)]
        let bytes = {
            let root_fd = match open_root(&self.inner.display_path) {
                Ok(fd) => fd,
                Err(error) => {
                    self.mark_stale();
                    return Err(error);
                },
            };
            let file = match open_relative(root_fd.as_raw_fd(), relative) {
                Ok(file) => file,
                Err(error) => {
                    self.mark_stale();
                    return Err(error);
                },
            };
            let metadata = match fstat_fd(file.as_raw_fd()) {
                Ok(metadata) => metadata,
                Err(error) => {
                    self.mark_stale();
                    return Err(error);
                },
            };
            if !is_regular_file(&metadata) {
                self.mark_stale();
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not a regular file", relative.display()),
                ));
            }
            let mut file = File::from(file);
            let mut bytes = Vec::new();
            if let Err(error) = file.read_to_end(&mut bytes) {
                self.mark_stale();
                return Err(error);
            }
            bytes
        };

        #[cfg(not(unix))]
        let bytes = {
            if std::fs::canonicalize(&self.inner.display_path).ok().as_deref()
                != Some(&self.inner.generation.canonical_path)
            {
                self.mark_stale();
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "specs directory generation was replaced",
                ));
            }
            let path = self.inner.display_path.join(relative);
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    self.mark_stale();
                    return Err(error);
                },
            };
            if !metadata.is_file() {
                self.mark_stale();
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not a regular file", relative.display()),
                ));
            }
            match std::fs::read(path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.mark_stale();
                    return Err(error);
                },
            }
        };

        if sha256_hex(&bytes) != *expected {
            self.mark_stale();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("snapshot digest mismatch: {}", relative.display()),
            ));
        }
        Ok(bytes)
    }

    pub(crate) fn read_to_string(&self, relative: &Path) -> io::Result<String> {
        let bytes = self.read_file(relative)?;
        String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    /// Read a file that may legitimately be absent from an older generation
    /// (for example `hook-modules.json`). An unexpected present file is not
    /// consumed; the next request will refresh the generation.
    pub(crate) fn read_optional_file(&self, relative: &Path) -> io::Result<Option<Vec<u8>>> {
        validate_relative_path(relative)?;
        if !self.inner.file_digests.contains_key(relative) {
            return Ok(None);
        }
        self.read_file(relative).map(Some)
    }

    /// Return names directly below a directory from the captured tree. This
    /// remains available even when the canonical directory is in a rename or
    /// cleanup window.
    pub(crate) fn read_dir(&self, relative: &Path) -> io::Result<Vec<std::ffi::OsString>> {
        validate_relative_path(relative)?;
        if self.kind(relative)? != EntryKind::Directory {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{} is not a directory", relative.display()),
            ));
        }
        let mut names = Vec::new();
        for path in self.inner.entries.keys() {
            if path.as_os_str().is_empty() || path == relative {
                continue;
            }
            if path.parent().unwrap_or_else(|| Path::new("")) == relative
                && let Some(name) = path.file_name()
            {
                names.push(name.to_os_string());
            }
        }
        Ok(names)
    }

    pub(crate) fn kind(&self, relative: &Path) -> io::Result<EntryKind> {
        validate_relative_path(relative)?;
        self.inner.entries.get(relative).copied().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("snapshot entry not found: {}", relative.display()),
            )
        })
    }

    pub(crate) fn is_file(&self, relative: &Path) -> bool {
        matches!(self.kind(relative), Ok(EntryKind::File))
    }

    /// Iterate over the digests captured from the opened directory generation.
    /// Pair verification uses these values instead of reopening and hashing the
    /// whole tree a second time. Lazy reads still verify their bytes against the
    /// same map before returning them.
    pub(crate) fn file_digests(&self) -> impl Iterator<Item = (&Path, &str)> {
        self.inner
            .file_digests
            .iter()
            .map(|(path, digest)| (path.as_path(), digest.as_str()))
    }

    pub(crate) fn mark_stale(&self) {
        self.inner.stale.store(true, Ordering::Release);
    }

    pub(crate) fn is_stale(&self) -> bool {
        self.inner.stale.load(Ordering::Acquire)
    }

    /// Detect an atomic replacement (or an in-place marker update) before a
    /// request starts. A changed generation is refreshable; the current
    /// request may still use identical files, while different files fail in
    /// `read_file` and force the next request to rebuild.
    pub(crate) fn generation_changed(&self) -> bool {
        #[cfg(unix)]
        {
            let current = open_root(&self.inner.display_path)
                .and_then(|fd| fstat_fd(fd.as_raw_fd()).map(|stat| root_identity(&stat)));
            if current.as_ref().ok() != Some(&self.inner.generation.root_identity) {
                return true;
            }
        }

        #[cfg(not(unix))]
        if std::fs::canonicalize(&self.inner.display_path).ok().as_deref()
            != Some(&self.inner.generation.canonical_path)
        {
            return true;
        }

        let Some(expected) = self.inner.generation.marker_digest.as_ref() else {
            return false;
        };
        match self.read_file(Path::new(".spec-pair.json")) {
            Ok(bytes) => sha256_hex(&bytes) != *expected,
            Err(_) => true,
        }
    }
}

#[cfg(unix)]
fn open_root(path: &Path) -> io::Result<OwnedFd> {
    let bytes = path.as_os_str().as_bytes();
    let path = CString::new(bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("directory path contains NUL: {error}"),
        )
    })?;
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY;
    let fd = unsafe { libc::open(path.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(unix)]
fn open_relative(parent_fd: i32, relative: &Path) -> io::Result<OwnedFd> {
    let mut current = duplicate_fd(parent_fd)?;
    let components = relative_components(relative)?;
    for (index, component) in components.iter().enumerate() {
        let name = CString::new(component.as_slice()).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("relative path contains NUL: {error}"),
            )
        })?;
        let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        if index + 1 < components.len() {
            flags |= libc::O_DIRECTORY;
        }
        let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        current = unsafe { OwnedFd::from_raw_fd(fd) };
    }
    Ok(current)
}

#[cfg(unix)]
#[allow(clippy::verbose_file_reads)]
fn capture_tree(
    parent_fd: i32,
    current: &Path,
    entries: &mut HashMap<PathBuf, EntryKind>,
    file_digests: &mut HashMap<PathBuf, String>,
) -> io::Result<()> {
    for name in read_dir_fd(parent_fd)? {
        let bytes = name.as_os_str().as_bytes();
        let c_name = CString::new(bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("directory entry contains NUL: {error}"),
            )
        })?;
        let fd = unsafe {
            libc::openat(
                parent_fd,
                c_name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let metadata = fstat_fd(fd.as_raw_fd())?;
        let relative = current.join(&name);
        if is_directory(&metadata) {
            entries.insert(relative.clone(), EntryKind::Directory);
            capture_tree(fd.as_raw_fd(), &relative, entries, file_digests)?;
        } else if is_regular_file(&metadata) {
            let mut file = File::from(fd);
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            entries.insert(relative.clone(), EntryKind::File);
            file_digests.insert(relative, sha256_hex(&bytes));
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot contains a special entry",
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn read_dir_fd(fd: i32) -> io::Result<Vec<std::ffi::OsString>> {
    let duplicate = duplicate_fd(fd)?;
    let raw = duplicate.into_raw_fd();
    let dir = unsafe { libc::fdopendir(raw) };
    if dir.is_null() {
        unsafe {
            libc::close(raw);
        }
        return Err(io::Error::last_os_error());
    }
    let mut names = Vec::new();
    loop {
        // POSIX reports both end-of-directory and errors as a null pointer.
        // Clear errno immediately before each call so a null result can be
        // classified without inheriting an unrelated earlier error.
        nix::errno::Errno::clear();
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            let errno = nix::errno::Errno::last_raw();
            if errno != 0 {
                unsafe {
                    libc::closedir(dir);
                }
                return Err(io::Error::from_raw_os_error(errno));
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(std::ffi::OsString::from_vec(name.to_vec()));
        }
    }
    unsafe {
        libc::closedir(dir);
    }
    Ok(names)
}

#[cfg(unix)]
fn duplicate_fd(fd: i32) -> io::Result<OwnedFd> {
    let duplicate = unsafe { libc::dup(fd) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

#[cfg(unix)]
fn fstat_fd(fd: i32) -> io::Result<libc::stat> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { stat.assume_init() })
}

#[cfg(unix)]
fn root_identity(stat: &libc::stat) -> RootIdentity {
    RootIdentity {
        // `st_dev` is already `u64` on Linux and a narrower type on some other Unix
        // targets. Keep the cast so the identity stays a `u64` everywhere.
        #[allow(clippy::unnecessary_cast)]
        device: stat.st_dev as u64,
        inode: stat.st_ino,
    }
}

#[cfg(unix)]
fn is_regular_file(stat: &libc::stat) -> bool {
    (stat.st_mode & libc::S_IFMT) == libc::S_IFREG
}

#[cfg(unix)]
fn is_directory(stat: &libc::stat) -> bool {
    (stat.st_mode & libc::S_IFMT) == libc::S_IFDIR
}

#[cfg(unix)]
fn relative_components(path: &Path) -> io::Result<Vec<Vec<u8>>> {
    let mut components = Vec::new();
    for component in path.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsafe relative path: {}", path.display()),
            ));
        };
        let bytes = name.as_bytes();
        if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&0) || bytes.contains(&b'/') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsafe relative path: {}", path.display()),
            ));
        }
        components.push(bytes.to_vec());
    }
    Ok(components)
}

#[cfg(not(unix))]
fn capture_tree_path(
    root: &Path,
    current: &Path,
    entries: &mut HashMap<PathBuf, EntryKind>,
    file_digests: &mut HashMap<PathBuf, String>,
) -> io::Result<()> {
    for entry in std::fs::read_dir(root.join(current))? {
        let entry = entry?;
        let relative = current.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            entries.insert(relative.clone(), EntryKind::Directory);
            capture_tree_path(root, &relative, entries, file_digests)?;
        } else if metadata.is_file() {
            entries.insert(relative.clone(), EntryKind::File);
            file_digests.insert(relative, sha256_hex(&std::fs::read(entry.path())?));
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot contains a special entry",
            ));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_relative_path(path: &Path) -> io::Result<()> {
    for component in path.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsafe relative path: {}", path.display()),
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn validate_relative_path(path: &Path) -> io::Result<()> {
    relative_components(path).map(|_| ())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        output.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        output.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
    }
    output
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn open_fd_count() -> Option<usize> {
        std::fs::read_dir("/dev/fd").ok().map(|entries| entries.count())
    }

    #[test]
    fn snapshot_does_not_retain_one_descriptor_per_file() {
        let root = tempfile::tempdir().expect("snapshot root");
        for index in 0..256 {
            std::fs::write(root.path().join(format!("{index}.json")), format!("{index}\n")).expect("snapshot file");
        }
        let Some(before) = open_fd_count() else {
            return;
        };
        let snapshot = DirectorySnapshot::open(root.path()).expect("snapshot");
        let Some(during) = open_fd_count() else {
            drop(snapshot);
            return;
        };
        drop(snapshot);

        // DirectorySnapshot uses transient openat descriptors while capturing
        // the digest map. None may remain proportional to the file count.
        assert!(
            during.saturating_sub(before) < 64,
            "snapshot retained too many descriptors: before={before}, during={during}"
        );
    }
}
