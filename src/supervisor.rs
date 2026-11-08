//! Process supervisor running outside the async runtime.
//!
//! The supervisor reads a handful of atomics and never takes a lock. The registry publishes a
//! single packed word holding the earliest outstanding deadline and its kind, so every snapshot
//! is coherent by construction. Participants arm a transaction marker before acquiring the
//! registry lock, so a stall inside any transaction, including the gap before publication, is
//! itself detected. Forced termination calls `process::exit` without joining threads.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::diag::{Diag, Event, Trigger};
use crate::limits;

/// Bits of the packed deadline word that hold nanoseconds since process start.
pub const NANOS_BITS: u32 = 56;
/// Largest encodable nanosecond value: about 834 days.
pub const MAX_ENCODABLE_NANOS: u64 = (1u64 << NANOS_BITS) - 1;
/// Supported uptime. Reaching it latches `UptimeLimit` so the encoding never wraps.
pub const UPTIME_LIMIT_NANOS: u64 = 800 * 24 * 60 * 60 * 1_000_000_000;

/// Fatal and non-fatal causes. Exit codes are derived from the cause category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Cause {
    None = 0,
    // Protocol violations: exit code 3.
    FrameOversized = 1,
    DuplicateId = 2,
    AdmissionOverflow = 3,
    ErrorFlood = 4,
    OutboundTooLarge = 5,
    // Timing, I/O, and invariant failures: exit code 4.
    DeliveryDeadline = 6,
    AdmissionDeadline = 7,
    AcceptanceDeadline = 8,
    RegistryStall = 9,
    StartupStall = 10,
    WriteError = 11,
    ReadError = 12,
    Invariant = 13,
    UptimeLimit = 14,
    // Initialization failure reported by the SDK: exit code 5.
    InitializeFailed = 15,
    /// A workspace worker exceeded its step budget or its job grace: exit code 4.
    WorkerStall = 16,
}

impl Cause {
    pub fn from_u8(v: u8) -> Cause {
        match v {
            1 => Cause::FrameOversized,
            2 => Cause::DuplicateId,
            3 => Cause::AdmissionOverflow,
            4 => Cause::ErrorFlood,
            5 => Cause::OutboundTooLarge,
            6 => Cause::DeliveryDeadline,
            7 => Cause::AdmissionDeadline,
            8 => Cause::AcceptanceDeadline,
            9 => Cause::RegistryStall,
            10 => Cause::StartupStall,
            11 => Cause::WriteError,
            12 => Cause::ReadError,
            13 => Cause::Invariant,
            14 => Cause::UptimeLimit,
            15 => Cause::InitializeFailed,
            16 => Cause::WorkerStall,
            _ => Cause::None,
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            Cause::None => 0,
            Cause::FrameOversized
            | Cause::DuplicateId
            | Cause::AdmissionOverflow
            | Cause::ErrorFlood
            | Cause::OutboundTooLarge => 3,
            Cause::DeliveryDeadline
            | Cause::AdmissionDeadline
            | Cause::AcceptanceDeadline
            | Cause::RegistryStall
            | Cause::StartupStall
            | Cause::WriteError
            | Cause::ReadError
            | Cause::Invariant
            | Cause::UptimeLimit
            | Cause::WorkerStall => 4,
            Cause::InitializeFailed => 5,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Cause::None => "none",
            Cause::FrameOversized => "frame_oversized",
            Cause::DuplicateId => "duplicate_id",
            Cause::AdmissionOverflow => "admission_overflow",
            Cause::ErrorFlood => "error_flood",
            Cause::OutboundTooLarge => "outbound_too_large",
            Cause::DeliveryDeadline => "delivery_deadline",
            Cause::AdmissionDeadline => "admission_deadline",
            Cause::AcceptanceDeadline => "acceptance_deadline",
            Cause::RegistryStall => "registry_stall",
            Cause::StartupStall => "startup_stall",
            Cause::WriteError => "write_error",
            Cause::ReadError => "read_error",
            Cause::Invariant => "invariant",
            Cause::UptimeLimit => "uptime_limit",
            Cause::InitializeFailed => "initialize_failed",
            Cause::WorkerStall => "worker_stall",
        }
    }
}

/// Kind tag stored in the low byte of the packed deadline word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DeadlineKind {
    Acceptance = 1,
    Admission = 2,
    Delivery = 3,
}

impl DeadlineKind {
    pub fn cause(self) -> Cause {
        match self {
            DeadlineKind::Acceptance => Cause::AcceptanceDeadline,
            DeadlineKind::Admission => Cause::AdmissionDeadline,
            DeadlineKind::Delivery => Cause::DeliveryDeadline,
        }
    }

    fn from_tag(tag: u8) -> Option<DeadlineKind> {
        match tag {
            1 => Some(DeadlineKind::Acceptance),
            2 => Some(DeadlineKind::Admission),
            3 => Some(DeadlineKind::Delivery),
            _ => None,
        }
    }
}

/// Packs a deadline into one word. Returns `None` when the value exceeds the encodable range.
pub fn encode(nanos: u64, kind: DeadlineKind) -> Option<u64> {
    if nanos > MAX_ENCODABLE_NANOS {
        return None;
    }
    Some((nanos << 8) | kind as u64)
}

/// Unpacks a deadline word. Zero means no deadline.
pub fn decode(word: u64) -> Option<(u64, DeadlineKind)> {
    if word == 0 {
        return None;
    }
    let kind = DeadlineKind::from_tag((word & 0xff) as u8)?;
    Some((word >> 8, kind))
}

/// Lock participants with their own transaction markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Participant {
    Reader = 0,
    Writer = 1,
    Runtime = 2,
    Startup = 3,
}

/// First marker index owned by workspace workers; each worker owns two consecutive markers.
pub const WORKER_MARKERS_BASE: usize = 4;
/// Total marker count: four participants plus a step and a job marker per worker.
pub const MARKERS: usize = WORKER_MARKERS_BASE + 2 * limits::WORKERS;

/// Marker index for a worker's current step.
pub fn worker_step_marker(worker: usize) -> usize {
    WORKER_MARKERS_BASE + 2 * worker
}

/// Marker index for a worker's current job.
pub fn worker_job_marker(worker: usize) -> usize {
    WORKER_MARKERS_BASE + 2 * worker + 1
}

/// State shared between the registry, the supervisor, and `main`.
pub struct Shared {
    start: Instant,
    next_deadline: AtomicU64,
    shutdown_deadline: AtomicU64,
    txn: [AtomicU64; MARKERS],
    fatal_cause: AtomicU8,
    orderly: AtomicU8,
    ct: CancellationToken,
    diag: Diag,
}

/// A coherent view of the supervised state at one instant.
#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    pub now: u64,
    pub next_deadline: u64,
    pub shutdown_deadline: u64,
    pub txn: [u64; MARKERS],
    pub fatal: Cause,
    pub orderly: Trigger,
}

/// What the supervisor decided for one tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Continue,
    Latch(Cause),
    Terminate(i32),
}

impl Shared {
    pub fn new(ct: CancellationToken, diag: Diag) -> Shared {
        Shared {
            start: Instant::now(),
            next_deadline: AtomicU64::new(0),
            shutdown_deadline: AtomicU64::new(0),
            txn: std::array::from_fn(|_| AtomicU64::new(0)),
            fatal_cause: AtomicU8::new(0),
            orderly: AtomicU8::new(0),
            ct,
            diag,
        }
    }

    pub fn token(&self) -> CancellationToken {
        self.ct.clone()
    }

    pub fn start(&self) -> Instant {
        self.start
    }

    /// Nanoseconds since process start on the monotonic clock.
    pub fn now_nanos(&self) -> u64 {
        self.start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
    }

    pub fn instant_to_nanos(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.start)
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64
    }

    /// Arms a transaction marker. Must precede the lock acquisition it protects.
    pub fn arm_txn(&self, p: Participant, budget: Duration) {
        let deadline = self.now_nanos().saturating_add(budget.as_nanos() as u64);
        self.txn[p as usize].store(deadline, Ordering::SeqCst);
    }

    /// Clears a transaction marker after the lock is released.
    pub fn clear_txn(&self, p: Participant) {
        self.txn[p as usize].store(0, Ordering::SeqCst);
    }

    /// True while a participant's marker is armed.
    pub fn txn_armed(&self, p: Participant) -> bool {
        self.txn[p as usize].load(Ordering::SeqCst) != 0
    }

    /// Arms a worker marker with a budget measured from now (step markers).
    pub fn arm_marker(&self, index: usize, budget: Duration) {
        let deadline = self.now_nanos().saturating_add(budget.as_nanos() as u64);
        self.txn[index].store(deadline.max(1), Ordering::SeqCst);
    }

    /// Arms a worker marker at an absolute instant (job markers).
    pub fn arm_marker_at(&self, index: usize, at: Instant) {
        self.txn[index].store(self.instant_to_nanos(at).max(1), Ordering::SeqCst);
    }

    /// Clears a worker marker.
    pub fn clear_marker(&self, index: usize) {
        self.txn[index].store(0, Ordering::SeqCst);
    }

    /// Publishes the earliest outstanding deadline as one packed word.
    pub fn publish_next_deadline(&self, earliest: Option<(Instant, DeadlineKind)>) {
        let word = match earliest {
            None => 0,
            Some((at, kind)) => match encode(self.instant_to_nanos(at), kind) {
                Some(w) => w,
                None => {
                    // Checked encoding: a deadline beyond the encodable range cannot be
                    // represented, so the process declares the uptime limit reached.
                    self.latch_fatal(Cause::UptimeLimit);
                    0
                }
            },
        };
        self.next_deadline.store(word, Ordering::SeqCst);
    }

    /// Latches the first fatal cause, starts the shutdown clock, and cancels the service.
    pub fn latch_fatal(&self, cause: Cause) -> bool {
        debug_assert!(cause != Cause::None);
        let won = self
            .fatal_cause
            .compare_exchange(0, cause as u8, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        if won {
            self.diag.emit(Event::Fatal {
                cause: cause.as_str(),
                code: cause.exit_code(),
            });
            self.start_shutdown_clock();
            self.ct.cancel();
        }
        won
    }

    /// Records the first orderly trigger and starts the shutdown clock. Never touches the fatal latch.
    pub fn trigger_orderly(&self, trigger: Trigger) {
        let _ = self
            .orderly
            .compare_exchange(0, trigger as u8, Ordering::SeqCst, Ordering::SeqCst);
        self.start_shutdown_clock();
    }

    fn start_shutdown_clock(&self) {
        let deadline = self
            .now_nanos()
            .saturating_add(limits::SHUTDOWN.as_nanos() as u64);
        let _ = self.shutdown_deadline.compare_exchange(
            0,
            deadline.max(1),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub fn fatal(&self) -> Cause {
        Cause::from_u8(self.fatal_cause.load(Ordering::SeqCst))
    }

    /// Shutdown deadline in nanoseconds since start, zero when no shutdown has been triggered.
    pub fn shutdown_deadline_nanos(&self) -> u64 {
        self.shutdown_deadline.load(Ordering::SeqCst)
    }

    pub fn is_fatal(&self) -> bool {
        self.fatal_cause.load(Ordering::SeqCst) != 0
    }

    pub fn orderly(&self) -> Trigger {
        match self.orderly.load(Ordering::SeqCst) {
            1 => Trigger::Eof,
            2 => Trigger::Sigint,
            3 => Trigger::Sigterm,
            _ => Trigger::None,
        }
    }

    /// Exit code rule: the first fatal cause wins; otherwise 0 for orderly completion and 1
    /// when the supervisor forces an orderly shutdown.
    pub fn exit_code(&self, forced: bool) -> i32 {
        exit_code(self.fatal(), forced)
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            now: self.now_nanos(),
            next_deadline: self.next_deadline.load(Ordering::SeqCst),
            shutdown_deadline: self.shutdown_deadline.load(Ordering::SeqCst),
            txn: std::array::from_fn(|i| self.txn[i].load(Ordering::SeqCst)),
            fatal: self.fatal(),
            orderly: self.orderly(),
        }
    }

    pub fn diag(&self) -> &Diag {
        &self.diag
    }
}

/// Pure exit-code rule.
pub fn exit_code(fatal: Cause, forced: bool) -> i32 {
    if fatal != Cause::None {
        fatal.exit_code()
    } else if forced {
        1
    } else {
        0
    }
}

/// Pure evaluation of one snapshot.
pub fn evaluate(s: &Snapshot) -> Decision {
    if s.shutdown_deadline != 0 && s.now >= s.shutdown_deadline {
        return Decision::Terminate(exit_code(s.fatal, true));
    }
    if s.fatal != Cause::None {
        return Decision::Continue;
    }
    if s.now >= UPTIME_LIMIT_NANOS {
        return Decision::Latch(Cause::UptimeLimit);
    }
    if s.txn[Participant::Startup as usize] != 0 && s.now >= s.txn[Participant::Startup as usize] {
        return Decision::Latch(Cause::StartupStall);
    }
    for p in [
        Participant::Reader,
        Participant::Writer,
        Participant::Runtime,
    ] {
        let marker = s.txn[p as usize];
        if marker != 0 && s.now >= marker {
            return Decision::Latch(Cause::RegistryStall);
        }
    }
    for marker in &s.txn[WORKER_MARKERS_BASE..MARKERS] {
        if *marker != 0 && s.now >= *marker {
            return Decision::Latch(Cause::WorkerStall);
        }
    }
    if let Some((nanos, kind)) = decode(s.next_deadline)
        && s.now >= nanos
    {
        return Decision::Latch(kind.cause());
    }
    Decision::Continue
}

/// Final exit mechanism. Production exits the process; tests record the decision.
pub trait Terminator: Send + Sync {
    fn terminate(&self, code: i32, cause: Cause);
}

pub struct ProcessExit;

impl Terminator for ProcessExit {
    fn terminate(&self, code: i32, _cause: Cause) {
        std::process::exit(code)
    }
}

/// Spawns the supervisor thread. Returns after `terminate` has been called once.
pub fn spawn(
    shared: Arc<Shared>,
    terminator: Arc<dyn Terminator>,
    tick: Duration,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("token-saver-supervisor".into())
        .spawn(move || {
            loop {
                std::thread::sleep(tick);
                let s = shared.snapshot();
                match evaluate(&s) {
                    Decision::Continue => {}
                    Decision::Latch(cause) => {
                        shared.latch_fatal(cause);
                    }
                    Decision::Terminate(code) => {
                        shared.diag().emit(Event::Shutdown {
                            trigger: s.orderly,
                            in_flight: 0,
                            code,
                        });
                        terminator.terminate(code, s.fatal);
                        return;
                    }
                }
            }
        })
        .expect("spawn supervisor thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(now: u64) -> Snapshot {
        Snapshot {
            now,
            next_deadline: 0,
            shutdown_deadline: 0,
            txn: [0; MARKERS],
            fatal: Cause::None,
            orderly: Trigger::None,
        }
    }

    #[test]
    fn encode_roundtrip_and_bounds() {
        assert_eq!(decode(0), None);
        let w = encode(123, DeadlineKind::Delivery).unwrap();
        assert_eq!(decode(w), Some((123, DeadlineKind::Delivery)));
        let max = encode(MAX_ENCODABLE_NANOS, DeadlineKind::Acceptance).unwrap();
        assert_eq!(
            decode(max),
            Some((MAX_ENCODABLE_NANOS, DeadlineKind::Acceptance))
        );
        assert_eq!(
            encode(MAX_ENCODABLE_NANOS + 1, DeadlineKind::Admission),
            None
        );
        assert_eq!(encode(u64::MAX, DeadlineKind::Admission), None);
        const { assert!(UPTIME_LIMIT_NANOS < MAX_ENCODABLE_NANOS) };
        // A deadline word never decodes to an absent deadline unless it is zero.
        assert!(decode(encode(1, DeadlineKind::Delivery).unwrap()).is_some());
    }

    #[test]
    fn idle_runs_forever() {
        assert_eq!(evaluate(&snap(0)), Decision::Continue);
        assert_eq!(evaluate(&snap(UPTIME_LIMIT_NANOS - 1)), Decision::Continue);
    }

    #[test]
    fn uptime_limit_latches_before_encoding_range_ends() {
        assert_eq!(
            evaluate(&snap(UPTIME_LIMIT_NANOS)),
            Decision::Latch(Cause::UptimeLimit)
        );
    }

    #[test]
    fn deadlines_and_markers_fire_in_priority_order() {
        let mut s = snap(10);
        s.next_deadline = encode(5, DeadlineKind::Acceptance).unwrap();
        assert_eq!(evaluate(&s), Decision::Latch(Cause::AcceptanceDeadline));
        s.txn[Participant::Reader as usize] = 9;
        assert_eq!(evaluate(&s), Decision::Latch(Cause::RegistryStall));
        s.txn[Participant::Startup as usize] = 8;
        assert_eq!(evaluate(&s), Decision::Latch(Cause::StartupStall));
        s.next_deadline = encode(11, DeadlineKind::Delivery).unwrap();
        s.txn = [0; MARKERS];
        assert_eq!(evaluate(&s), Decision::Continue);
    }

    #[test]
    fn worker_markers_latch_worker_stall_after_registry_markers() {
        let mut s = snap(10);
        s.txn[worker_step_marker(0)] = 9;
        assert_eq!(evaluate(&s), Decision::Latch(Cause::WorkerStall));
        s.txn[worker_job_marker(limits::WORKERS - 1)] = 5;
        assert_eq!(evaluate(&s), Decision::Latch(Cause::WorkerStall));
        s.txn[Participant::Writer as usize] = 7;
        assert_eq!(evaluate(&s), Decision::Latch(Cause::RegistryStall));
        s.txn = [0; MARKERS];
        s.txn[worker_job_marker(1)] = 11;
        assert_eq!(evaluate(&s), Decision::Continue);
        assert_eq!(Cause::WorkerStall.exit_code(), 4);
        assert_eq!(Cause::from_u8(16), Cause::WorkerStall);
        assert_eq!(Cause::WorkerStall.as_str(), "worker_stall");
    }

    #[test]
    fn shutdown_deadline_terminates_with_latched_or_forced_code() {
        let mut s = snap(100);
        s.shutdown_deadline = 50;
        assert_eq!(evaluate(&s), Decision::Terminate(1));
        s.fatal = Cause::DeliveryDeadline;
        assert_eq!(evaluate(&s), Decision::Terminate(4));
        s.fatal = Cause::DuplicateId;
        assert_eq!(evaluate(&s), Decision::Terminate(3));
    }

    #[test]
    fn fatal_latch_is_first_fatal_only_and_survives_orderly() {
        let shared = Shared::new(CancellationToken::new(), Diag::disabled());
        shared.trigger_orderly(Trigger::Eof);
        assert_eq!(shared.exit_code(false), 0);
        assert_eq!(shared.exit_code(true), 1);
        assert!(shared.latch_fatal(Cause::WriteError));
        assert!(!shared.latch_fatal(Cause::DuplicateId));
        shared.trigger_orderly(Trigger::Sigint);
        assert_eq!(shared.fatal(), Cause::WriteError);
        assert_eq!(shared.exit_code(false), 4);
        assert_eq!(shared.exit_code(true), 4);
        assert_eq!(shared.orderly(), Trigger::Eof);
        assert!(shared.token().is_cancelled());
    }
}
