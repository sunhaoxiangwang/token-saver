//! Allowlisted diagnostics.
//!
//! Every event has a fixed name and fixed, typed fields. No event carries a request id,
//! a client-provided method string, a reason text, or any payload. Events are queued to a
//! bounded channel drained by one thread that owns stderr; protocol work never blocks on
//! stderr. Over the rate limit, events are dropped and counted.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::limits;

/// Diagnostic verbosity. `Off` still emits `startup`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Off = 0,
    Error = 1,
    Info = 2,
    Debug = 3,
}

impl Level {
    pub fn parse(s: &str) -> Option<Level> {
        match s {
            "off" => Some(Level::Off),
            "error" => Some(Level::Error),
            "info" => Some(Level::Info),
            "debug" => Some(Level::Debug),
            _ => None,
        }
    }
}

/// Known MCP methods, for diagnostics only. Anything else is `Other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Discover,
    Initialize,
    Ping,
    ToolsList,
    ToolsCall,
    Other,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Discover => "discover",
            Method::Initialize => "initialize",
            Method::Ping => "ping",
            Method::ToolsList => "tools_list",
            Method::ToolsCall => "tools_call",
            Method::Other => "other",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    Parse,
    Envelope,
    IdLength,
    Params,
    Version,
    Method,
}

impl RejectReason {
    fn as_str(self) -> &'static str {
        match self {
            RejectReason::Parse => "parse",
            RejectReason::Envelope => "envelope",
            RejectReason::IdLength => "id_length",
            RejectReason::Params => "params",
            RejectReason::Version => "version",
            RejectReason::Method => "method",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    ToolError,
    RpcError,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::ToolError => "tool_error",
            Outcome::RpcError => "rpc_error",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationDrop {
    UnknownId,
    Unsupported,
    NoCredit,
    Outstanding,
    ClientResponse,
    Malformed,
}

impl NotificationDrop {
    fn as_str(self) -> &'static str {
        match self {
            NotificationDrop::UnknownId => "unknown_id",
            NotificationDrop::Unsupported => "unsupported",
            NotificationDrop::NoCredit => "no_credit",
            NotificationDrop::Outstanding => "outstanding",
            NotificationDrop::ClientResponse => "client_response",
            NotificationDrop::Malformed => "malformed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    None,
    Eof,
    Sigint,
    Sigterm,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::None => "none",
            Trigger::Eof => "eof",
            Trigger::Sigint => "sigint",
            Trigger::Sigterm => "sigterm",
        }
    }
}

/// The complete set of diagnostic events.
#[derive(Clone, Debug)]
pub enum Event {
    Startup {
        version: &'static str,
        instance_id: String,
        supported_versions: &'static str,
    },
    FrameRejected {
        reason: RejectReason,
        bytes: usize,
    },
    FrameOversized {
        bytes_seen: usize,
    },
    RequestAccepted {
        method: Method,
        bytes: usize,
        requests: usize,
    },
    RequestAdmitted {
        method: Method,
        admitted: usize,
    },
    RequestSkipped {
        method: Method,
    },
    CancelAccepted {
        method: Method,
    },
    CancelLate {
        method: Method,
    },
    ResponseSent {
        method: Method,
        outcome: Outcome,
        code: i32,
        bytes: usize,
        handler_ms: Option<u64>,
        admit_to_delivery_ms: Option<u64>,
        seq: u64,
    },
    ResponseSuppressed {
        method: Method,
    },
    ResultOversized {
        bytes: usize,
    },
    OutboundOversized {
        bytes: usize,
    },
    OutboundUnexpected,
    NotificationDropped {
        reason: NotificationDrop,
    },
    WriteBegan {
        seq: u64,
        bytes: usize,
    },
    Fatal {
        cause: &'static str,
        code: i32,
    },
    Shutdown {
        trigger: Trigger,
        in_flight: usize,
        code: i32,
    },
    DiagDropped {
        count: u64,
    },
}

impl Event {
    fn level(&self) -> Level {
        match self {
            Event::Startup { .. } => Level::Off,
            Event::Fatal { .. }
            | Event::FrameOversized { .. }
            | Event::ResultOversized { .. }
            | Event::OutboundOversized { .. } => Level::Error,
            Event::Shutdown { .. }
            | Event::FrameRejected { .. }
            | Event::CancelAccepted { .. }
            | Event::CancelLate { .. }
            | Event::RequestSkipped { .. }
            | Event::ResponseSuppressed { .. }
            | Event::OutboundUnexpected
            | Event::NotificationDropped { .. }
            | Event::DiagDropped { .. } => Level::Info,
            Event::RequestAccepted { .. }
            | Event::RequestAdmitted { .. }
            | Event::ResponseSent { .. }
            | Event::WriteBegan { .. } => Level::Debug,
        }
    }

    fn write_line(&self, out: &mut dyn Write, ts_ms: u64) -> std::io::Result<()> {
        write!(out, "ts_ms={ts_ms} ")?;
        match self {
            Event::Startup {
                version,
                instance_id,
                supported_versions,
            } => {
                write!(
                    out,
                    "event=startup version={version} instance_id={instance_id} supported_versions={supported_versions}"
                )
            }
            Event::FrameRejected { reason, bytes } => {
                write!(
                    out,
                    "event=frame.rejected reason={} bytes={bytes}",
                    reason.as_str()
                )
            }
            Event::FrameOversized { bytes_seen } => {
                write!(out, "event=frame.oversized bytes_seen={bytes_seen}")
            }
            Event::RequestAccepted {
                method,
                bytes,
                requests,
            } => {
                write!(
                    out,
                    "event=request.accepted method={} bytes={bytes} requests={requests}",
                    method.as_str()
                )
            }
            Event::RequestAdmitted { method, admitted } => {
                write!(
                    out,
                    "event=request.admitted method={} admitted={admitted}",
                    method.as_str()
                )
            }
            Event::RequestSkipped { method } => {
                write!(out, "event=request.skipped method={}", method.as_str())
            }
            Event::CancelAccepted { method } => {
                write!(out, "event=cancel.accepted method={}", method.as_str())
            }
            Event::CancelLate { method } => {
                write!(out, "event=cancel.late method={}", method.as_str())
            }
            Event::ResponseSent {
                method,
                outcome,
                code,
                bytes,
                handler_ms,
                admit_to_delivery_ms,
                seq,
            } => {
                write!(
                    out,
                    "event=response.sent method={} outcome={} code={code} bytes={bytes} handler_ms={} admit_to_delivery_ms={} seq={seq}",
                    method.as_str(),
                    outcome.as_str(),
                    OptMs(*handler_ms),
                    OptMs(*admit_to_delivery_ms)
                )
            }
            Event::ResponseSuppressed { method } => {
                write!(out, "event=response.suppressed method={}", method.as_str())
            }
            Event::ResultOversized { bytes } => write!(out, "event=result.oversized bytes={bytes}"),
            Event::OutboundOversized { bytes } => {
                write!(out, "event=outbound.oversized bytes={bytes}")
            }
            Event::OutboundUnexpected => write!(out, "event=outbound.unexpected"),
            Event::NotificationDropped { reason } => {
                write!(out, "event=notification.dropped reason={}", reason.as_str())
            }
            Event::WriteBegan { seq, bytes } => {
                write!(out, "event=write.began seq={seq} bytes={bytes}")
            }
            Event::Fatal { cause, code } => write!(out, "event=fatal cause={cause} code={code}"),
            Event::Shutdown {
                trigger,
                in_flight,
                code,
            } => {
                write!(
                    out,
                    "event=shutdown trigger={} in_flight={in_flight} code={code}",
                    trigger.as_str()
                )
            }
            Event::DiagDropped { count } => write!(out, "event=diag.dropped count={count}"),
        }?;
        out.write_all(b"\n")
    }
}

struct OptMs(Option<u64>);

impl std::fmt::Display for OptMs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(v) => write!(f, "{v}"),
            None => f.write_str("-"),
        }
    }
}

struct Inner {
    tx: Option<SyncSender<Event>>,
    level: Level,
    start: Instant,
    dropped: AtomicU64,
    window: Mutex<(Instant, u32)>,
}

/// Cloneable handle to the diagnostics sink.
#[derive(Clone)]
pub struct Diag {
    inner: Arc<Inner>,
}

impl Diag {
    /// Starts the stderr writer thread and returns a handle.
    pub fn start(level: Level) -> Diag {
        let (tx, rx) = sync_channel::<Event>(limits::DIAG_QUEUE);
        let inner = Arc::new(Inner {
            tx: Some(tx),
            level,
            start: Instant::now(),
            dropped: AtomicU64::new(0),
            window: Mutex::new((Instant::now(), 0)),
        });
        let thread_inner = inner.clone();
        std::thread::Builder::new()
            .name("token-saver-diag".into())
            .spawn(move || writer_loop(rx, thread_inner))
            .expect("spawn diagnostics thread");
        Diag { inner }
    }

    /// A handle that records nothing. Used by tests that do not inspect diagnostics.
    pub fn disabled() -> Diag {
        Diag {
            inner: Arc::new(Inner {
                tx: None,
                level: Level::Off,
                start: Instant::now(),
                dropped: AtomicU64::new(0),
                window: Mutex::new((Instant::now(), 0)),
            }),
        }
    }

    /// Milliseconds since the diagnostics clock started.
    pub fn elapsed_ms(&self) -> u64 {
        self.inner.start.elapsed().as_millis() as u64
    }

    /// Enqueues an event without blocking. Drops are counted.
    pub fn emit(&self, event: Event) {
        let Some(tx) = self.inner.tx.as_ref() else {
            return;
        };
        if event.level() > self.inner.level {
            return;
        }
        if !self.within_rate() {
            self.inner.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        match tx.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.inner.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Number of events dropped so far, for tests.
    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    fn within_rate(&self) -> bool {
        let mut w = self.inner.window.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if now.duration_since(w.0) >= Duration::from_secs(1) {
            *w = (now, 0);
        }
        if w.1 >= limits::DIAG_PER_SECOND {
            return false;
        }
        w.1 += 1;
        true
    }
}

fn writer_loop(rx: Receiver<Event>, inner: Arc<Inner>) {
    let stderr = std::io::stderr();
    let mut out = stderr.lock();
    let mut last_summary = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(event) => {
                let ts = inner.start.elapsed().as_millis() as u64;
                if event.write_line(&mut out, ts).is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if last_summary.elapsed() >= Duration::from_secs(1) {
            last_summary = Instant::now();
            let dropped = inner.dropped.swap(0, Ordering::Relaxed);
            if dropped > 0 {
                let ts = inner.start.elapsed().as_millis() as u64;
                if (Event::DiagDropped { count: dropped })
                    .write_line(&mut out, ts)
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}
