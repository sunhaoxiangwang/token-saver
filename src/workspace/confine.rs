//! Descriptor-relative, no-follow, single-component resolution from a root descriptor.
//!
//! Authority contract (docs/adr/0002-retained-directory-authority.md): a job opens an object only
//! if, at the instant of the open, it is a direct child of a directory object the job holds;
//! symlinks and objects classified as special files are rejected before any open; an object that
//! becomes a special file between classification and open is opened non-blocking, identified by
//! `fstat`, and closed unread. Descriptors refer to objects, not pathnames.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read};
use std::ops::Deref;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, RawMode};
use rustix::io::Errno;

use super::policy::{
    ERR_INVALID_PATH, ERR_NOT_A_DIRECTORY, ERR_NOT_A_FILE, ERR_NOT_FOUND, ERR_NOT_READABLE,
};
use super::worker::{Stepper, Stop};

/// Object classification. Only regular files and directories are ever opened for content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Regular,
    Directory,
    Symlink,
    Other,
    Unknown,
}

impl Kind {
    fn from_file_type(ft: FileType) -> Kind {
        match ft {
            FileType::RegularFile => Kind::Regular,
            FileType::Directory => Kind::Directory,
            FileType::Symlink => Kind::Symlink,
            FileType::Unknown => Kind::Unknown,
            _ => Kind::Other,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Meta {
    pub kind: Kind,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: Vec<u8>,
    pub kind: Kind,
}

/// A directory iterator that yields bounded batches. `.` and `..` are never yielded.
pub trait DirIter: Send {
    /// Appends at most `max` entries to `out`. Returns false once the directory is exhausted.
    fn next_batch(&mut self, max: usize, out: &mut Vec<Entry>) -> io::Result<bool>;
}

/// The system calls the workers make. Production uses `RealFs`; tests wrap it to hold inside a
/// call (step marker armed) or at the checkpoint after a call (step marker clear).
pub trait FileSystem: Send + Sync {
    /// `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`.
    fn stat_at(&self, dir: BorrowedFd<'_>, name: &[u8]) -> io::Result<Meta>;
    /// `openat(dir, name, O_RDONLY|O_DIRECTORY|O_CLOEXEC|O_NOFOLLOW|O_NONBLOCK|O_NOCTTY)`.
    fn open_dir_at(&self, dir: BorrowedFd<'_>, name: &[u8]) -> io::Result<OwnedFd>;
    /// `openat(dir, name, O_RDONLY|O_CLOEXEC|O_NOFOLLOW|O_NONBLOCK|O_NOCTTY)`.
    fn open_file_at(&self, dir: BorrowedFd<'_>, name: &[u8]) -> io::Result<File>;
    fn fstat(&self, fd: BorrowedFd<'_>) -> io::Result<Meta>;
    fn clear_nonblock(&self, fd: BorrowedFd<'_>) -> io::Result<()>;
    /// One `read(2)` of at most `buf.len()` bytes.
    fn read(&self, file: &File, buf: &mut [u8]) -> io::Result<usize>;
    /// Opens an iterator over `dir`. The iterator owns one descriptor of its own.
    fn open_dir_iter(&self, dir: BorrowedFd<'_>) -> io::Result<Box<dyn DirIter>>;
    /// Called by the worker at the checkpoint after every step. Production: nothing.
    fn checkpoint(&self, _worker: usize, _step: u64) {}
}

const OPEN_COMMON: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::NOCTTY);

fn cstr(name: &[u8]) -> io::Result<CString> {
    CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in name"))
}

fn meta_of(st: &rustix::fs::Stat) -> Meta {
    Meta {
        kind: Kind::from_file_type(FileType::from_raw_mode(RawMode::from(st.st_mode))),
        size: u64::try_from(st.st_size).unwrap_or(0),
    }
}

/// The production implementation over rustix.
pub struct RealFs;

struct RealDirIter {
    dir: Dir,
}

impl DirIter for RealDirIter {
    fn next_batch(&mut self, max: usize, out: &mut Vec<Entry>) -> io::Result<bool> {
        let mut taken = 0;
        while taken < max {
            match self.dir.next() {
                None => return Ok(false),
                Some(Err(e)) => return Err(e.into()),
                Some(Ok(entry)) => {
                    let name = entry.file_name().to_bytes();
                    if name == b"." || name == b".." {
                        continue;
                    }
                    out.push(Entry {
                        name: name.to_vec(),
                        kind: Kind::from_file_type(entry.file_type()),
                    });
                    taken += 1;
                }
            }
        }
        Ok(true)
    }
}

impl FileSystem for RealFs {
    fn stat_at(&self, dir: BorrowedFd<'_>, name: &[u8]) -> io::Result<Meta> {
        let st = rustix::fs::statat(dir, cstr(name)?.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)?;
        Ok(meta_of(&st))
    }

    fn open_dir_at(&self, dir: BorrowedFd<'_>, name: &[u8]) -> io::Result<OwnedFd> {
        Ok(rustix::fs::openat(
            dir,
            cstr(name)?.as_c_str(),
            OPEN_COMMON.union(OFlags::DIRECTORY),
            Mode::empty(),
        )?)
    }

    fn open_file_at(&self, dir: BorrowedFd<'_>, name: &[u8]) -> io::Result<File> {
        let fd = rustix::fs::openat(dir, cstr(name)?.as_c_str(), OPEN_COMMON, Mode::empty())?;
        Ok(File::from(fd))
    }

    fn fstat(&self, fd: BorrowedFd<'_>) -> io::Result<Meta> {
        Ok(meta_of(&rustix::fs::fstat(fd)?))
    }

    fn clear_nonblock(&self, fd: BorrowedFd<'_>) -> io::Result<()> {
        let flags = rustix::fs::fcntl_getfl(fd)?;
        rustix::fs::fcntl_setfl(fd, flags.difference(OFlags::NONBLOCK))?;
        Ok(())
    }

    fn read(&self, file: &File, buf: &mut [u8]) -> io::Result<usize> {
        (&*file).read(buf)
    }

    fn open_dir_iter(&self, dir: BorrowedFd<'_>) -> io::Result<Box<dyn DirIter>> {
        let d = Dir::read_from(dir)?;
        Ok(Box::new(RealDirIter { dir: d }))
    }
}

/// Runs the root acquisition under startup supervision: the caller's `Startup` marker must be
/// armed, so a stalled filesystem lookup latches `startup_stall` (exit 4) like any other startup
/// stall. Returns an error without opening anything when the marker is not armed.
pub fn acquire_root<F>(shared: &crate::supervisor::Shared, open: F) -> io::Result<OwnedFd>
where
    F: FnOnce() -> io::Result<OwnedFd>,
{
    if !shared.txn_armed(crate::supervisor::Participant::Startup) {
        return Err(io::Error::other("startup marker not armed"));
    }
    open()
}

/// Opens the root directory once, at startup. The only use of ambient path authority.
pub fn open_root(path: &str) -> io::Result<OwnedFd> {
    if !path.starts_with('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "root must be absolute",
        ));
    }
    let fd = rustix::fs::open(path, OPEN_COMMON.union(OFlags::DIRECTORY), Mode::empty())?;
    let st = rustix::fs::fstat(&fd)?;
    if meta_of(&st).kind != Kind::Directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a directory",
        ));
    }
    Ok(fd)
}

/// A resource counted in `job_descriptors_open` for as long as it lives.
pub struct Tracked<T> {
    inner: T,
    counter: Arc<AtomicUsize>,
}

impl<T> Tracked<T> {
    pub fn new(inner: T, counter: Arc<AtomicUsize>) -> Tracked<T> {
        counter.fetch_add(1, Ordering::SeqCst);
        Tracked { inner, counter }
    }
}

impl<T> Deref for Tracked<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T> std::ops::DerefMut for Tracked<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T> Drop for Tracked<T> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Why a resolution or scan did not produce content.
#[derive(Debug)]
pub enum Fail {
    Tool(&'static str),
    Stop(Stop),
}

impl From<Stop> for Fail {
    fn from(s: Stop) -> Fail {
        Fail::Stop(s)
    }
}

/// A held directory: the root (borrowed for the process lifetime) or a child the job opened.
pub enum DirHandle<'r> {
    Root(BorrowedFd<'r>),
    Owned(Tracked<OwnedFd>),
}

impl DirHandle<'_> {
    pub fn fd(&self) -> BorrowedFd<'_> {
        match self {
            DirHandle::Root(fd) => *fd,
            DirHandle::Owned(fd) => fd.as_fd(),
        }
    }
}

/// Maps an I/O error from a component open or stat to the fixed tool error text.
pub fn map_io(e: &io::Error) -> &'static str {
    match Errno::from_io_error(e) {
        Some(Errno::LOOP) => ERR_INVALID_PATH,
        Some(Errno::NOENT) => ERR_NOT_FOUND,
        Some(Errno::NOTDIR) => ERR_NOT_A_DIRECTORY,
        // A directory where a file is required, or a raced socket (`EOPNOTSUPP` on macOS,
        // `ENXIO` on Linux) that cannot be opened for reading.
        Some(Errno::ISDIR) | Some(Errno::OPNOTSUPP) | Some(Errno::NXIO) => ERR_NOT_A_FILE,
        _ => ERR_NOT_READABLE,
    }
}

/// Classifies `name` inside `dir` without opening it.
pub fn classify(st: &mut Stepper, dir: BorrowedFd<'_>, name: &[u8]) -> Result<Meta, Fail> {
    st.step(|fs| fs.stat_at(dir, name))?
        .map_err(|e| Fail::Tool(map_io(&e)))
}

/// Opens a child directory: classification, non-blocking open, authoritative `fstat`.
pub fn open_subdir(
    st: &mut Stepper,
    dir: BorrowedFd<'_>,
    name: &[u8],
) -> Result<Tracked<OwnedFd>, Fail> {
    let meta = classify(st, dir, name)?;
    match meta.kind {
        Kind::Directory => {}
        Kind::Symlink => return Err(Fail::Tool(ERR_INVALID_PATH)),
        Kind::Regular | Kind::Other | Kind::Unknown => {
            return Err(Fail::Tool(ERR_NOT_A_DIRECTORY));
        }
    }
    let fd = st
        .step(|fs| fs.open_dir_at(dir, name))?
        .map_err(|e| Fail::Tool(map_io(&e)))?;
    let fd = st.track(fd);
    let after = st
        .step(|fs| fs.fstat(fd.as_fd()))?
        .map_err(|e| Fail::Tool(map_io(&e)))?;
    if after.kind != Kind::Directory {
        return Err(Fail::Tool(ERR_NOT_A_DIRECTORY));
    }
    Ok(fd)
}

/// Opens a regular file: classification, non-blocking open, authoritative `fstat` (anything but a
/// regular file is closed unread), then `O_NONBLOCK` is cleared. Returns the file and its size.
pub fn open_regular(
    st: &mut Stepper,
    dir: BorrowedFd<'_>,
    name: &[u8],
) -> Result<(Tracked<File>, u64), Fail> {
    let meta = classify(st, dir, name)?;
    match meta.kind {
        Kind::Regular => {}
        Kind::Symlink => return Err(Fail::Tool(ERR_INVALID_PATH)),
        Kind::Directory | Kind::Other | Kind::Unknown => {
            return Err(Fail::Tool(ERR_NOT_A_FILE));
        }
    }
    let file = st
        .step(|fs| fs.open_file_at(dir, name))?
        .map_err(|e| Fail::Tool(map_io(&e)))?;
    let file = st.track(file);
    let after = st
        .step(|fs| fs.fstat(file.as_fd()))?
        .map_err(|e| Fail::Tool(map_io(&e)))?;
    if after.kind != Kind::Regular {
        // A raced special file: opened non-blocking, identified, dropped here unread.
        return Err(Fail::Tool(ERR_NOT_A_FILE));
    }
    st.step(|fs| fs.clear_nonblock(file.as_fd()))?
        .map_err(|e| Fail::Tool(map_io(&e)))?;
    Ok((file, after.size))
}

/// Walks `comps` from `root`, each component a directory. The previous descriptor is dropped once
/// the next is open, so at most two component descriptors are live.
pub fn walk_dir<'r>(
    st: &mut Stepper,
    root: BorrowedFd<'r>,
    comps: &[&[u8]],
) -> Result<DirHandle<'r>, Fail> {
    let mut cur = DirHandle::Root(root);
    for comp in comps {
        let next = open_subdir(st, cur.fd(), comp)?;
        cur = DirHandle::Owned(next);
    }
    Ok(cur)
}

/// Lists a directory completely up to `limit` entries into `out` (cleared first), in batches of
/// `DIR_BATCH` per step. Returns whether entries beyond `limit` exist (the directory is truncated).
pub fn list_dir(
    st: &mut Stepper,
    dir: BorrowedFd<'_>,
    limit: usize,
    out: &mut Vec<Entry>,
) -> Result<bool, Fail> {
    out.clear();
    let iter = st
        .step(|fs| fs.open_dir_iter(dir))?
        .map_err(|e| Fail::Tool(map_io(&e)))?;
    let mut iter = st.track(iter);
    loop {
        let want = limit
            .saturating_sub(out.len())
            .min(crate::limits::DIR_BATCH);
        if want == 0 {
            // Probe for one more entry to report truncation without keeping it.
            let mut probe = Vec::with_capacity(1);
            let more = st
                .step(|_| iter.next_batch(1, &mut probe))?
                .map_err(|e| Fail::Tool(map_io(&e)))?;
            return Ok(more && !probe.is_empty() || !probe.is_empty());
        }
        let more = st
            .step(|_| iter.next_batch(want, out))?
            .map_err(|e| Fail::Tool(map_io(&e)))?;
        if !more {
            return Ok(false);
        }
    }
}
