//! Owned worker pool: job slots, steps with supervisor markers, orphan accounting.
//!
//! Every blocking system call is a step. The step marker is armed immediately before the call and
//! cleared immediately after; the job marker stays armed from job start until the permit is
//! released. Workers check the fatal latch, the cancellation flag, and the deadline before every
//! step. A step admitted before a latch may start or finish afterwards; once a worker observes the
//! latch it admits no further step (TS-M1-REVIEW-004).

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};

use super::confine::{Entry, FileSystem, Tracked};
use super::policy::{ERR_BUSY, ERR_INTERNAL};
use super::render::{BoundedWriter, TextBuilder, escaped_len, meta_line};
use super::{Args, outline, search, slice};
use crate::diag::{Diag, Event, JobKind};
use crate::limits;
use crate::supervisor::{Shared, worker_job_marker, worker_step_marker};

/// Why work stopped before completing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The handler gave up (deadline or client cancellation); the job is orphaned.
    Cancelled,
    /// The job's own deadline (or a tool's soft budget) passed; the job reports what it has.
    Deadline,
    /// A fatal cause is latched; no further step is admitted.
    Fatal,
}

/// Per-worker step bookkeeping: markers, checks, descriptor accounting.
pub struct Stepper {
    fs: Arc<dyn FileSystem>,
    shared: Arc<Shared>,
    worker: usize,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
    soft_deadline: Option<Instant>,
    steps: u64,
    descriptors: Arc<AtomicUsize>,
}

impl Stepper {
    pub fn fs(&self) -> &dyn FileSystem {
        &*self.fs
    }

    pub fn steps(&self) -> u64 {
        self.steps
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// A tool-specific budget shorter than the job deadline (search scanning).
    pub fn set_soft_deadline(&mut self, at: Instant) {
        self.soft_deadline = Some(at);
    }

    /// Admission check for a step or a unit of CPU work.
    pub fn check(&self) -> Result<(), Stop> {
        if self.shared.is_fatal() {
            return Err(Stop::Fatal);
        }
        if self.cancel.load(Ordering::SeqCst) {
            return Err(Stop::Cancelled);
        }
        let now = Instant::now();
        if now >= self.deadline || self.soft_deadline.is_some_and(|d| now >= d) {
            return Err(Stop::Deadline);
        }
        Ok(())
    }

    /// Runs one blocking call as a step: check, arm the step marker, call, clear, checkpoint.
    pub fn step<T>(
        &mut self,
        f: impl FnOnce(&dyn FileSystem) -> io::Result<T>,
    ) -> Result<io::Result<T>, Stop> {
        self.check()?;
        let marker = worker_step_marker(self.worker);
        self.shared.arm_marker(marker, limits::STEP);
        let result = f(&*self.fs);
        self.shared.clear_marker(marker);
        self.steps += 1;
        self.fs.checkpoint(self.worker, self.steps);
        Ok(result)
    }

    /// Wraps a descriptor-bearing resource so it is counted while it lives.
    pub fn track<T>(&self, inner: T) -> Tracked<T> {
        Tracked::new(inner, self.descriptors.clone())
    }
}

/// A queued directory during traversal: its name and its parent in the arena.
#[derive(Clone, Debug)]
pub struct Node {
    pub parent: u32,
    pub depth: u16,
    pub name: Vec<u8>,
}

/// Fixed per-worker storage, allocated once and reused.
pub struct Buffers {
    pub read: Vec<u8>,
    pub file: Vec<u8>,
    pub text: TextBuilder,
    pub result: Vec<u8>,
    pub listing: Vec<Entry>,
    pub arena: Vec<Node>,
    pub line: Vec<u8>,
}

/// Bytes of a returned line kept in memory; longer lines are counted, not stored.
pub const LINE_STORE_BYTES: usize = 4 * limits::SLICE_LINE_CHARS + 4;

impl Buffers {
    pub fn new() -> Buffers {
        Buffers {
            read: vec![0; limits::READ_STEP_BYTES],
            file: vec![0; limits::SEARCH_FILE_BYTES],
            text: TextBuilder::with_capacity(limits::MAX_WORKSPACE_RESULT_BYTES),
            result: Vec::with_capacity(limits::MAX_OUTBOUND_FRAME_BYTES),
            listing: Vec::with_capacity(limits::DIR_ENTRY_LIMIT),
            arena: Vec::with_capacity(limits::OUTLINE_RAW_ENTRY_LIMIT),
            line: Vec::with_capacity(LINE_STORE_BYTES),
        }
    }

    /// Clears tree-dependent storage between jobs; capacities are kept.
    pub fn reset(&mut self) {
        self.listing.clear();
        self.arena.clear();
        self.line.clear();
        self.result.clear();
    }

    /// Capacities, asserted unchanged by tests before and after jobs.
    pub fn capacities(&self) -> [usize; 6] {
        [
            self.read.capacity(),
            self.file.capacity(),
            self.text.capacity(),
            self.result.capacity(),
            self.listing.capacity(),
            self.arena.capacity(),
        ]
    }
}

impl Default for Buffers {
    fn default() -> Self {
        Buffers::new()
    }
}

/// Published counts.
pub struct Counts {
    pub queued: AtomicUsize,
    pub running: AtomicUsize,
    pub orphaned: AtomicUsize,
    pub descriptors: Arc<AtomicUsize>,
    /// Queued plus running, attached to the registry so orderly completion waits for jobs.
    pub jobs: Arc<AtomicUsize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub jobs_queued: usize,
    pub jobs_running: usize,
    pub jobs_orphaned: usize,
    pub job_slots_available: usize,
    pub job_descriptors_open: usize,
}

const PHASE_ACTIVE: u8 = 0;
const PHASE_ORPHANED: u8 = 1;
const PHASE_DONE: u8 = 2;

struct Job {
    kind: JobKind,
    args: Args,
    deadline: Instant,
    cancel: Arc<AtomicBool>,
    phase: Arc<AtomicU8>,
    reply: oneshot::Sender<CallToolResult>,
    _permit: OwnedSemaphorePermit,
}

/// Handler-side guard: dropped before completion, it cancels the job and marks it orphaned.
struct Handle {
    kind: JobKind,
    cancel: Arc<AtomicBool>,
    phase: Arc<AtomicU8>,
    counts: Arc<Counts>,
    diag: Diag,
    done: bool,
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        self.cancel.store(true, Ordering::SeqCst);
        if self
            .phase
            .compare_exchange(
                PHASE_ACTIVE,
                PHASE_ORPHANED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            self.counts.orphaned.fetch_add(1, Ordering::SeqCst);
            self.diag.emit(Event::JobOrphaned { kind: self.kind });
        }
    }
}

/// The pool: four workers, eight slots, one root descriptor.
pub struct Pool {
    tx: Mutex<SyncSender<Job>>,
    slots: Arc<Semaphore>,
    counts: Arc<Counts>,
    diag: Diag,
}

impl Pool {
    pub fn start(
        fs: Arc<dyn FileSystem>,
        root: std::os::fd::OwnedFd,
        shared: Arc<Shared>,
        diag: Diag,
    ) -> Arc<Pool> {
        let (tx, rx) = sync_channel::<Job>(limits::JOB_SLOTS);
        let rx = Arc::new(Mutex::new(rx));
        let counts = Arc::new(Counts {
            queued: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            orphaned: AtomicUsize::new(0),
            descriptors: Arc::new(AtomicUsize::new(0)),
            jobs: Arc::new(AtomicUsize::new(0)),
        });
        let root = Arc::new(root);
        for worker in 0..limits::WORKERS {
            let rx = rx.clone();
            let fs = fs.clone();
            let shared = shared.clone();
            let diag = diag.clone();
            let counts = counts.clone();
            let root = root.clone();
            std::thread::Builder::new()
                .name(format!("token-saver-worker-{worker}"))
                .spawn(move || worker_loop(worker, rx, fs, root, shared, diag, counts))
                .expect("spawn worker thread");
        }
        Arc::new(Pool {
            tx: Mutex::new(tx),
            slots: Arc::new(Semaphore::new(limits::JOB_SLOTS)),
            counts,
            diag,
        })
    }

    pub fn jobs_counter(&self) -> Arc<AtomicUsize> {
        self.counts.jobs.clone()
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            jobs_queued: self.counts.queued.load(Ordering::SeqCst),
            jobs_running: self.counts.running.load(Ordering::SeqCst),
            jobs_orphaned: self.counts.orphaned.load(Ordering::SeqCst),
            job_slots_available: self.slots.available_permits(),
            job_descriptors_open: self.counts.descriptors.load(Ordering::SeqCst),
        }
    }

    /// Acquires a slot (within `SLOT_WAIT`, else `busy`), enqueues the job, and awaits its reply.
    /// Dropping the returned future after enqueue orphans the job.
    pub async fn execute(
        &self,
        kind: JobKind,
        args: Args,
        deadline: Instant,
    ) -> Result<CallToolResult, &'static str> {
        let permit =
            match tokio::time::timeout(limits::SLOT_WAIT, self.slots.clone().acquire_owned()).await
            {
                Ok(Ok(permit)) => permit,
                Ok(Err(_)) => return Err(ERR_INTERNAL),
                Err(_) => {
                    self.diag.emit(Event::JobBusy { kind });
                    return Err(ERR_BUSY);
                }
            };
        let cancel = Arc::new(AtomicBool::new(false));
        let phase = Arc::new(AtomicU8::new(PHASE_ACTIVE));
        let (reply_tx, reply_rx) = oneshot::channel();
        let job = Job {
            kind,
            args,
            deadline,
            cancel: cancel.clone(),
            phase: phase.clone(),
            reply: reply_tx,
            _permit: permit,
        };
        self.counts.queued.fetch_add(1, Ordering::SeqCst);
        self.counts.jobs.fetch_add(1, Ordering::SeqCst);
        let sent = self
            .tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_send(job);
        if let Err(e) = sent {
            self.counts.queued.fetch_sub(1, Ordering::SeqCst);
            self.counts.jobs.fetch_sub(1, Ordering::SeqCst);
            match e {
                TrySendError::Full(job) | TrySendError::Disconnected(job) => drop(job),
            }
            return Err(ERR_INTERNAL);
        }
        let mut handle = Handle {
            kind,
            cancel,
            phase,
            counts: self.counts.clone(),
            diag: self.diag.clone(),
            done: false,
        };
        match reply_rx.await {
            Ok(result) => {
                handle.done = true;
                Ok(result)
            }
            Err(_) => {
                handle.done = true;
                Err(ERR_INTERNAL)
            }
        }
    }
}

fn worker_loop(
    worker: usize,
    rx: Arc<Mutex<Receiver<Job>>>,
    fs: Arc<dyn FileSystem>,
    root: Arc<std::os::fd::OwnedFd>,
    shared: Arc<Shared>,
    diag: Diag,
    counts: Arc<Counts>,
) {
    let mut buffers = Buffers::new();
    loop {
        let job = {
            let guard = rx.lock().unwrap_or_else(|e| e.into_inner());
            guard.recv()
        };
        let Ok(job) = job else {
            return;
        };
        counts.queued.fetch_sub(1, Ordering::SeqCst);
        counts.running.fetch_add(1, Ordering::SeqCst);
        diag.emit(Event::JobStarted { kind: job.kind });
        let started = Instant::now();
        let job_marker = worker_job_marker(worker);
        shared.arm_marker_at(job_marker, job.deadline + limits::JOB_GRACE);
        let mut stepper = Stepper {
            fs: fs.clone(),
            shared: shared.clone(),
            worker,
            cancel: job.cancel.clone(),
            deadline: job.deadline,
            soft_deadline: None,
            steps: 0,
            descriptors: counts.descriptors.clone(),
        };
        let outcome = run_tool(&job.args, &mut stepper, root.as_ref(), &mut buffers, &diag);
        buffers.reset();
        shared.clear_marker(job_marker);
        let steps = stepper.steps();
        let ms = started.elapsed().as_millis() as u64;
        drop(stepper);
        let Job {
            kind,
            phase,
            reply,
            _permit,
            ..
        } = job;
        let orphaned = phase
            .compare_exchange(PHASE_ACTIVE, PHASE_DONE, Ordering::SeqCst, Ordering::SeqCst)
            .is_err();
        match outcome {
            Ok((result, stop_reason)) => {
                diag.emit(Event::JobFinished {
                    kind,
                    stop_reason,
                    steps,
                    ms,
                });
                if !orphaned {
                    let _ = reply.send(result);
                }
            }
            Err(stop) => {
                let stop_reason = match stop {
                    Stop::Cancelled => "cancelled",
                    Stop::Deadline => "time_budget",
                    Stop::Fatal => "fatal",
                };
                diag.emit(Event::JobFinished {
                    kind,
                    stop_reason,
                    steps,
                    ms,
                });
            }
        }
        if orphaned {
            counts.orphaned.fetch_sub(1, Ordering::SeqCst);
            diag.emit(Event::JobOrphanEnded { kind });
        }
        counts.running.fetch_sub(1, Ordering::SeqCst);
        counts.jobs.fetch_sub(1, Ordering::SeqCst);
        drop(_permit);
    }
}

/// Runs one job to a result (or a stop that leaves no result).
fn run_tool(
    args: &Args,
    st: &mut Stepper,
    root: &std::os::fd::OwnedFd,
    b: &mut Buffers,
    diag: &Diag,
) -> Result<(CallToolResult, &'static str), Stop> {
    use std::os::fd::AsFd;
    let root = root.as_fd();
    let built = match args {
        Args::Outline(a) => outline::run(a, st, root, b)?,
        Args::Slice(a) => slice::run(a, st, root, b)?,
        Args::Search(a) => search::run(a, st, root, b)?,
    };
    Ok(match built {
        Built::Error(message) => (tool_error(message), "tool_error"),
        Built::Result {
            text,
            meta,
            stop_reason,
        } => {
            let result = finish(text, meta, b, diag);
            (result, stop_reason)
        }
    })
}

/// What a tool produced.
pub enum Built {
    Error(&'static str),
    Result {
        text: String,
        meta: Value,
        stop_reason: &'static str,
    },
}

pub fn tool_error(message: &'static str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// Assembles the text-only result (content, then the metadata line) and verifies its exact
/// serialized size inside the bounded result buffer.
fn finish(text: String, meta: Value, b: &mut Buffers, diag: &Diag) -> CallToolResult {
    let mut full = text;
    if !full.is_empty() {
        full.push('\n');
    }
    full.push_str(&meta_line(&meta));
    let result = CallToolResult::success(vec![ContentBlock::text(full)]);
    let (ok, bytes) = {
        let mut w = BoundedWriter::new(&mut b.result);
        let ok = serde_json::to_writer(&mut w, &result).is_ok();
        (ok, w.len())
    };
    if !ok || bytes > limits::MAX_WORKSPACE_RESULT_BYTES {
        diag.emit(Event::ResultOversized { bytes });
        return tool_error(ERR_INTERNAL);
    }
    result
}

/// Metadata maxima per tool: the escaped cost of the metadata line with every integer at its
/// widest and every enumeration at its longest value, plus its newline.
pub fn metadata_max(kind: JobKind) -> usize {
    static MAX: std::sync::OnceLock<[usize; 3]> = std::sync::OnceLock::new();
    let all = MAX.get_or_init(|| {
        [
            escaped_len(&meta_line(&outline::max_metadata())) + 2,
            escaped_len(&meta_line(&slice::max_metadata())) + 2,
            escaped_len(&meta_line(&search::max_metadata())) + 2,
        ]
    });
    match kind {
        JobKind::Outline => all[0],
        JobKind::Slice => all[1],
        JobKind::Search => all[2],
    }
}

/// Grace used by tests to compute expected timelines.
pub fn job_grace() -> Duration {
    limits::JOB_GRACE
}

/// Test seam: runs one job on the calling thread with a fresh stepper over `shared`, so tests can
/// assert buffer capacities and descriptor counts around a job. Not used by production.
#[doc(hidden)]
pub fn run_job_direct(
    fs: Arc<dyn FileSystem>,
    shared: Arc<Shared>,
    root: std::os::fd::BorrowedFd<'_>,
    args: &Args,
    buffers: &mut Buffers,
    deadline: Instant,
    diag: &Diag,
) -> Result<(CallToolResult, &'static str, usize), Stop> {
    let descriptors = Arc::new(AtomicUsize::new(0));
    let mut st = Stepper {
        fs,
        shared,
        worker: 0,
        cancel: Arc::new(AtomicBool::new(false)),
        deadline,
        soft_deadline: None,
        steps: 0,
        descriptors: descriptors.clone(),
    };
    use std::os::fd::AsFd;
    let built = match args {
        Args::Outline(a) => outline::run(a, &mut st, root.as_fd(), buffers)?,
        Args::Slice(a) => slice::run(a, &mut st, root.as_fd(), buffers)?,
        Args::Search(a) => search::run(a, &mut st, root.as_fd(), buffers)?,
    };
    let out = match built {
        Built::Error(message) => (tool_error(message), "tool_error"),
        Built::Result {
            text,
            meta,
            stop_reason,
        } => (finish(text, meta, buffers, diag), stop_reason),
    };
    buffers.reset();
    Ok((out.0, out.1, descriptors.load(Ordering::SeqCst)))
}
