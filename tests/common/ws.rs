//! Workspace test support: a gated `FileSystem` that can hold inside a call (step marker armed)
//! or at the checkpoint after it (step marker clear, job marker armed), and an in-process harness
//! serving the production handler with a workspace over production transport and supervisor.

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use rmcp::ServiceExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use token_saver::diag::Diag;
use token_saver::limits;
use token_saver::probe::{TokenSaver, new_instance_id};
use token_saver::supervisor::{self, Cause, Shared};
use token_saver::transport::{BoundedStdio, Registry};
use token_saver::workspace::Workspace;
use token_saver::workspace::confine::{DirIter, Entry, FileSystem, Meta, RealFs, open_root};

use super::fixture::RecordingTerminator;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Call {
    StatAt,
    OpenDir,
    OpenFile,
    Fstat,
    ClearNonblock,
    Read,
    OpenDirIter,
    NextBatch,
}

/// One programmed hold. Engaged once, on the first matching call.
pub struct Hold {
    call: Call,
    name: Option<Vec<u8>>,
    inside: bool,
    /// Matching calls to let pass before engaging.
    skip: Mutex<usize>,
    state: Mutex<HoldState>,
    cv: Condvar,
    used: Mutex<bool>,
}

#[derive(Default)]
struct HoldState {
    engaged_at: Option<Instant>,
    released: bool,
}

impl Hold {
    /// Waits until the hold has engaged; returns the instant it did.
    pub fn wait_engaged(&self, timeout: Duration) -> Option<Instant> {
        let deadline = Instant::now() + timeout;
        let mut g = self.state.lock().unwrap();
        while g.engaged_at.is_none() {
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (ng, _) = self.cv.wait_timeout(g, deadline - now).unwrap();
            g = ng;
        }
        g.engaged_at
    }

    pub fn engaged_at(&self) -> Option<Instant> {
        self.state.lock().unwrap().engaged_at
    }

    pub fn release(&self) {
        self.state.lock().unwrap().released = true;
        self.cv.notify_all();
    }

    fn block(&self) {
        let mut g = self.state.lock().unwrap();
        if g.engaged_at.is_none() {
            g.engaged_at = Some(Instant::now());
        }
        self.cv.notify_all();
        while !g.released {
            g = self.cv.wait(g).unwrap();
        }
    }
}

struct Inner {
    real: RealFs,
    holds: Mutex<Vec<Arc<Hold>>>,
    pending_checkpoint: Mutex<HashMap<ThreadId, Arc<Hold>>>,
    calls: Mutex<HashMap<Call, usize>>,
    delay: Mutex<Option<Duration>>,
    allocation_marker: AtomicUsize,
    force_unknown: AtomicBool,
    max_read_len: AtomicUsize,
}

/// `RealFs` with programmable holds and an optional per-step delay.
#[derive(Clone)]
pub struct GatedFs {
    inner: Arc<Inner>,
}

impl Default for GatedFs {
    fn default() -> Self {
        GatedFs::new()
    }
}

impl GatedFs {
    pub fn new() -> GatedFs {
        GatedFs {
            inner: Arc::new(Inner {
                real: RealFs,
                holds: Mutex::new(Vec::new()),
                pending_checkpoint: Mutex::new(HashMap::new()),
                calls: Mutex::new(HashMap::new()),
                delay: Mutex::new(None),
                allocation_marker: AtomicUsize::new(0),
                force_unknown: AtomicBool::new(false),
                max_read_len: AtomicUsize::new(0),
            }),
        }
    }

    /// Holds inside the first matching call, while its step marker is armed (hold (a)).
    pub fn hold_inside(&self, call: Call, name: Option<&[u8]>) -> Arc<Hold> {
        self.add(call, name, true, 0)
    }

    /// Holds inside the n-th matching call (1-based).
    pub fn hold_inside_nth(&self, call: Call, name: Option<&[u8]>, n: usize) -> Arc<Hold> {
        self.add(call, name, true, n.saturating_sub(1))
    }

    /// Holds at the checkpoint after the first matching call, with the step marker clear and the
    /// job marker armed (hold (b)).
    pub fn hold_after(&self, call: Call, name: Option<&[u8]>) -> Arc<Hold> {
        self.add(call, name, false, 0)
    }

    /// Holds at the checkpoint after the n-th matching call (1-based).
    pub fn hold_after_nth(&self, call: Call, name: Option<&[u8]>, n: usize) -> Arc<Hold> {
        self.add(call, name, false, n.saturating_sub(1))
    }

    /// Reports every directory entry's type as `Unknown`, forcing the `statat` fallback.
    pub fn set_force_unknown(&self, on: bool) {
        self.inner.force_unknown.store(on, Ordering::SeqCst);
    }

    /// Sleeps this long inside every call (slow but cooperative work).
    pub fn set_delay(&self, d: Duration) {
        *self.inner.delay.lock().unwrap() = Some(d);
    }

    /// Largest buffer any `read` call received.
    pub fn max_read_len(&self) -> usize {
        self.inner.max_read_len.load(Ordering::SeqCst)
    }

    pub fn calls(&self, call: Call) -> usize {
        *self.inner.calls.lock().unwrap().get(&call).unwrap_or(&0)
    }

    fn add(&self, call: Call, name: Option<&[u8]>, inside: bool, skip: usize) -> Arc<Hold> {
        let hold = Arc::new(Hold {
            call,
            name: name.map(|n| n.to_vec()),
            inside,
            skip: Mutex::new(skip),
            state: Mutex::new(HoldState::default()),
            cv: Condvar::new(),
            used: Mutex::new(false),
        });
        self.inner.holds.lock().unwrap().push(hold.clone());
        hold
    }

    /// Called before each delegated call: counts it, applies holds inside, arms checkpoint holds.
    fn before(&self, call: Call, name: Option<&[u8]>) {
        *self.inner.calls.lock().unwrap().entry(call).or_insert(0) += 1;
        if let Some(d) = *self.inner.delay.lock().unwrap() {
            std::thread::sleep(d);
        }
        let matched: Option<Arc<Hold>> = {
            let holds = self.inner.holds.lock().unwrap();
            let mut found = None;
            for h in holds.iter() {
                if h.call != call
                    || !h.name.as_deref().is_none_or(|n| Some(n) == name)
                    || *h.used.lock().unwrap()
                {
                    continue;
                }
                let mut skip = h.skip.lock().unwrap();
                if *skip > 0 {
                    *skip -= 1;
                    continue;
                }
                found = Some(h.clone());
                break;
            }
            found
        };
        let Some(h) = matched else {
            return;
        };
        *h.used.lock().unwrap() = true;
        if h.inside {
            h.block();
        } else {
            self.inner
                .pending_checkpoint
                .lock()
                .unwrap()
                .insert(std::thread::current().id(), h);
        }
    }
}

struct GatedDirIter {
    inner: Box<dyn DirIter>,
    fs: GatedFs,
}

impl DirIter for GatedDirIter {
    fn next_batch(&mut self, max: usize, out: &mut Vec<Entry>) -> std::io::Result<bool> {
        self.fs.before(Call::NextBatch, None);
        let before = out.len();
        let more = self.inner.next_batch(max, out)?;
        if self.fs.inner.force_unknown.load(Ordering::SeqCst) {
            for e in &mut out[before..] {
                e.kind = token_saver::workspace::confine::Kind::Unknown;
            }
        }
        Ok(more)
    }
}

impl FileSystem for GatedFs {
    fn stat_at(&self, dir: BorrowedFd<'_>, name: &[u8]) -> std::io::Result<Meta> {
        self.before(Call::StatAt, Some(name));
        self.inner.real.stat_at(dir, name)
    }
    fn open_dir_at(&self, dir: BorrowedFd<'_>, name: &[u8]) -> std::io::Result<OwnedFd> {
        self.before(Call::OpenDir, Some(name));
        self.inner.real.open_dir_at(dir, name)
    }
    fn open_file_at(&self, dir: BorrowedFd<'_>, name: &[u8]) -> std::io::Result<File> {
        self.before(Call::OpenFile, Some(name));
        self.inner.real.open_file_at(dir, name)
    }
    fn fstat(&self, fd: BorrowedFd<'_>) -> std::io::Result<Meta> {
        self.before(Call::Fstat, None);
        self.inner.real.fstat(fd)
    }
    fn clear_nonblock(&self, fd: BorrowedFd<'_>) -> std::io::Result<()> {
        self.before(Call::ClearNonblock, None);
        self.inner.real.clear_nonblock(fd)
    }
    fn read(&self, file: &File, buf: &mut [u8]) -> std::io::Result<usize> {
        self.before(Call::Read, None);
        self.inner
            .max_read_len
            .fetch_max(buf.len(), Ordering::SeqCst);
        self.inner.real.read(file, buf)
    }
    fn open_dir_iter(&self, dir: BorrowedFd<'_>) -> std::io::Result<Box<dyn DirIter>> {
        self.before(Call::OpenDirIter, None);
        let inner = self.inner.real.open_dir_iter(dir)?;
        Ok(Box::new(GatedDirIter {
            inner,
            fs: self.clone(),
        }))
    }
    fn checkpoint(&self, _worker: usize, _step: u64) {
        let pending = self
            .inner
            .pending_checkpoint
            .lock()
            .unwrap()
            .remove(&std::thread::current().id());
        if let Some(h) = pending {
            h.block();
        }
    }
}

/// In-process harness serving the production handler with a workspace over the gated filesystem.
pub struct WsHarness {
    pub registry: Arc<Registry>,
    pub shared: Arc<Shared>,
    pub terminator: Arc<RecordingTerminator>,
    pub workspace: Arc<Workspace>,
    pub fs: GatedFs,
    client_in: Option<std::io::PipeWriter>,
    client_out: Receiver<Vec<u8>>,
    service_done: Receiver<()>,
}

impl WsHarness {
    pub fn start(root: &Path, fs: GatedFs) -> WsHarness {
        let diag = Diag::disabled();
        let ct = CancellationToken::new();
        let shared = Arc::new(Shared::new(ct.clone(), diag.clone()));
        let terminator = Arc::new(RecordingTerminator::default());
        supervisor::spawn(shared.clone(), terminator.clone(), limits::TICK);
        let registry = Registry::new(shared.clone(), diag.clone());
        let root_fd = open_root(root.to_str().unwrap()).expect("open root");
        let workspace = Arc::new(Workspace::with_fs(
            Arc::new(fs.clone()),
            root_fd,
            shared.clone(),
            diag.clone(),
        ));
        registry.attach_jobs(workspace.jobs_counter());
        let (client_to_server_r, client_to_server_w) = std::io::pipe().expect("pipe");
        let (server_to_client_r, server_to_client_w) = std::io::pipe().expect("pipe");
        let transport =
            BoundedStdio::spawn(client_to_server_r, server_to_client_w, registry.clone());
        let handler =
            TokenSaver::new(registry.clone(), new_instance_id()).with_workspace(workspace.clone());
        let (done_tx, done_rx) = channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                if let Ok(running) = handler.serve_with_ct(transport, ct).await {
                    let _ = running.waiting().await;
                }
            });
            let _ = done_tx.send(());
        });
        let client_out = pump(server_to_client_r);
        WsHarness {
            registry,
            shared,
            terminator,
            workspace,
            fs,
            client_in: Some(client_to_server_w),
            client_out,
            service_done: done_rx,
        }
    }

    pub fn send(&mut self, value: Value) {
        if let Some(w) = self.client_in.as_mut() {
            let _ = w.write_all(value.to_string().as_bytes());
            let _ = w.write_all(b"\n");
            let _ = w.flush();
        }
    }

    pub fn recv(&mut self, timeout: Duration) -> Option<Value> {
        let line = self.client_out.recv_timeout(timeout).ok()?;
        Some(serde_json::from_slice(&line).expect("server output is JSON"))
    }

    pub fn recv_expect(&mut self) -> Value {
        self.recv(Duration::from_secs(8))
            .expect("expected a frame within 8 s")
    }

    pub fn assert_silent(&mut self, wait: Duration) {
        if let Some(v) = self.recv(wait) {
            panic!("unexpected frame: {v}");
        }
    }

    pub fn close_stdin(&mut self) {
        self.client_in.take();
    }

    pub fn wait_service_end(&self, timeout: Duration) -> bool {
        self.service_done.recv_timeout(timeout).is_ok()
    }

    pub fn wait_fatal(&self, timeout: Duration) -> Cause {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let c = self.shared.fatal();
            if c != Cause::None {
                return c;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Cause::None
    }

    /// Waits until the predicate over (registry counts, pool snapshot) holds.
    pub fn wait_until(
        &self,
        timeout: Duration,
        pred: impl Fn(token_saver::transport::Counts, token_saver::workspace::worker::Snapshot) -> bool,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if pred(self.registry.counts(), self.workspace.snapshot()) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// Admission instant of the most recent `request.admitted`, approximated by the time the
    /// registry first reports the given admitted count.
    pub fn wait_admitted(&self, n: usize, timeout: Duration) -> Option<Instant> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.registry.counts().admitted >= n {
                return Some(Instant::now());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    }
}

fn pump<R: Read + Send + 'static>(reader: R) -> Receiver<Vec<u8>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let mut buf = Vec::new();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(buf).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

/// The metadata line of a workspace result text, parsed to a JSON object.
pub fn meta_of(text: &str) -> Value {
    let last = text.rsplit('\n').next().unwrap_or("");
    token_saver::workspace::render::parse_meta_line(last)
        .unwrap_or_else(|| panic!("no metadata line in {text:?}"))
}

/// The content of a workspace result text without its metadata line.
pub fn content_of(text: &str) -> String {
    match text.rfind('\n') {
        Some(idx) if text[idx + 1..].starts_with('#') => text[..idx].to_owned(),
        None if text.starts_with('#') => String::new(),
        _ => text.to_owned(),
    }
}

/// Builds a `tools/call` for a workspace tool in the modern era.
pub fn ws_call(id: Value, tool: &str, arguments: Value) -> Value {
    serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"_meta":super::meta("2026-07-28"),"name":tool,"arguments":arguments}})
}

/// A fresh temporary directory under the target directory, removed on drop.
pub struct TempRoot {
    pub path: std::path::PathBuf,
}

impl TempRoot {
    pub fn new(tag: &str) -> TempRoot {
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/tmp");
        std::fs::create_dir_all(&base).unwrap();
        // Short names: Unix socket paths created inside the root are limited to about 100 bytes.
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let short: String = tag.chars().take(4).collect();
        let path = base.join(format!("w{}-{short}{n}", std::process::id() % 100_000));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempRoot { path }
    }

    pub fn write(&self, rel: &str, content: &[u8]) {
        let p = self.path.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }

    pub fn mkdir(&self, rel: &str) {
        std::fs::create_dir_all(self.path.join(rel)).unwrap();
    }

    /// Creates nested directories component by component, so the total path may exceed the
    /// platform `PATH_MAX`; returns a descriptor of the deepest directory.
    pub fn mkdir_deep(&self, comps: &[&str]) -> OwnedFd {
        use rustix::fs::{Mode, OFlags, mkdirat, openat};
        let mut cur = rustix::fs::open(
            &self.path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        for c in comps {
            match mkdirat(&cur, *c, Mode::from_raw_mode(0o755)) {
                Ok(()) => {}
                Err(rustix::io::Errno::EXIST) => {}
                Err(e) => panic!("mkdirat {c:?}: {e}"),
            }
            cur = openat(
                &cur,
                *c,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .unwrap();
        }
        cur
    }

    /// Writes a file below a directory descriptor obtained from `mkdir_deep`.
    pub fn write_at(dir: &OwnedFd, name: &str, content: &[u8]) {
        use rustix::fs::{Mode, OFlags, openat};
        let fd = openat(
            dir,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o644),
        )
        .unwrap();
        let mut f = File::from(fd);
        f.write_all(content).unwrap();
    }

    pub fn symlink(&self, target: &str, rel: &str) {
        std::os::unix::fs::symlink(target, self.path.join(rel)).unwrap();
    }

    /// A sibling directory outside the root, removed with it.
    pub fn outside(&self) -> std::path::PathBuf {
        let p = self.path.with_file_name(format!(
            "{}-outside",
            self.path.file_name().unwrap().to_str().unwrap()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
        let _ = std::fs::remove_dir_all(self.outside());
    }
}

/// Opens a root descriptor for direct (non-harness) runs.
pub fn root_fd(path: &Path) -> OwnedFd {
    open_root(path.to_str().unwrap()).expect("open root")
}

pub fn borrowed(fd: &OwnedFd) -> BorrowedFd<'_> {
    fd.as_fd()
}
