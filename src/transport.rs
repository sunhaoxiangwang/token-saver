//! Bounded stdio transport with a request registry.
//!
//! Ownership model (see docs/transport.md):
//! - The reader thread frames stdin, validates envelopes independently of the SDK, reserves
//!   every accepted request id in the registry before publishing it, applies client
//!   cancellations directly, and builds protocol-error frames under the error budget. It never
//!   blocks on the async side.
//! - The adapter implements `rmcp::transport::Transport`. `receive` admits one queued request per
//!   permit; `send` is the commitment point and creates a delivery record before publication.
//! - The writer thread writes and flushes each frame, then retires its record, which releases
//!   the permit it held.
//! - The supervisor, outside the runtime, enforces every budget from the published deadline word.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use rmcp::model::{
    ClientJsonRpcMessage, ClientNotification, ClientRequest, ErrorCode, ErrorData, JsonRpcMessage,
    NumberOrString, ServerJsonRpcMessage, ServerResult,
};
use rmcp::service::{RoleServer, RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::diag::{Diag, Event, Method, NotificationDrop, Outcome, RejectReason, Trigger};
use crate::limits;
use crate::supervisor::{Cause, DeadlineKind, Participant, Shared};

pub type RequestId = NumberOrString;

// ---------------------------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------------------------

enum SlotState {
    Queued,
    QueuedCancelled,
    Admitted {
        admit_at: Instant,
        permit: OwnedSemaphorePermit,
    },
    Cancelled {
        admit_at: Instant,
        _permit: OwnedSemaphorePermit,
    },
    Committed {
        admit_at: Instant,
        permit: OwnedSemaphorePermit,
        seq: u64,
    },
    Rejected {
        seq: u64,
    },
}

impl SlotState {
    fn counts_as_request(&self) -> bool {
        !matches!(self, SlotState::Rejected { .. })
    }

    fn is_admitted(&self) -> bool {
        matches!(
            self,
            SlotState::Admitted { .. } | SlotState::Cancelled { .. } | SlotState::Committed { .. }
        )
    }
}

struct Slot {
    method: Method,
    accepted_at: Instant,
    state: SlotState,
    token: CancellationToken,
    handler_start: Option<Instant>,
    handler_end: Option<Instant>,
}

enum Owner {
    Request(RequestId),
    Correlated(RequestId),
    Uncorrelated,
}

struct Delivery {
    owner: Owner,
    commit_at: Instant,
    bytes: usize,
    // Dropped at acknowledgment, which releases the error budget.
    _error_permit: Option<OwnedSemaphorePermit>,
    method: Method,
    outcome: Outcome,
    code: i32,
}

#[derive(Default)]
struct RegistryInner {
    slots: HashMap<RequestId, Slot>,
    deliveries: BTreeMap<u64, Delivery>,
    request_count: usize,
    next_seq: u64,
    initialize_committed: u64,
    initialized_forwarded: u64,
    initialized_outstanding: u64,
}

impl RegistryInner {
    fn next_seq(&mut self) -> u64 {
        self.next_seq += 1;
        self.next_seq
    }

    fn admitted_count(&self) -> usize {
        self.slots
            .values()
            .filter(|s| s.state.is_admitted())
            .count()
    }
}

/// Context handed to the handler for one admitted request.
#[derive(Clone)]
pub struct HandlerContext {
    pub token: CancellationToken,
    pub admit_at: Instant,
}

/// Result of dispatching a queued request.
#[derive(Debug, PartialEq, Eq)]
pub enum Dispatch {
    Admit,
    Skip,
}

/// Result of committing an outbound response.
#[derive(Debug, PartialEq, Eq)]
pub enum Commit {
    Deliver(u64),
    Suppress,
    Unexpected,
}

/// Snapshot of registry counters, for tests and shutdown diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub requests: usize,
    pub admitted: usize,
    pub deliveries: usize,
    pub initialized_outstanding: u64,
    pub available_permits: usize,
    pub available_error_budget: usize,
    /// Workspace jobs queued or running (zero without `--root`).
    pub jobs: usize,
}

pub struct Registry {
    inner: Mutex<RegistryInner>,
    shared: Arc<Shared>,
    admission: Arc<Semaphore>,
    error_budget: Arc<Semaphore>,
    diag: Diag,
    /// Queued plus running workspace jobs, published by the worker pool. Orderly completion
    /// requires it to be zero, so a job that outlived its handler delays exit 0 until it ends.
    jobs: OnceLock<Arc<AtomicUsize>>,
}

impl Registry {
    pub fn new(shared: Arc<Shared>, diag: Diag) -> Arc<Registry> {
        Arc::new(Registry {
            inner: Mutex::new(RegistryInner::default()),
            shared,
            admission: Arc::new(Semaphore::new(limits::MAX_ADMITTED)),
            error_budget: Arc::new(Semaphore::new(limits::ERROR_BUDGET)),
            diag,
            jobs: OnceLock::new(),
        })
    }

    /// Attaches the worker pool's job counter. Called once at startup when `--root` is given.
    pub fn attach_jobs(&self, counter: Arc<AtomicUsize>) {
        let _ = self.jobs.set(counter);
    }

    fn jobs(&self) -> usize {
        self.jobs
            .get()
            .map(|c| c.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn diag(&self) -> &Diag {
        &self.diag
    }

    pub fn admission(&self) -> Arc<Semaphore> {
        self.admission.clone()
    }

    /// Latches a fatal cause. Idempotent.
    pub fn fatal(&self, cause: Cause) {
        self.shared.latch_fatal(cause);
    }

    /// Runs one registry transaction: arm marker, lock, mutate, publish, unlock, clear marker.
    fn transact<R>(&self, p: Participant, f: impl FnOnce(&mut RegistryInner, &Diag) -> R) -> R {
        self.shared.arm_txn(p, limits::TRANSACTION);
        let result = {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let r = f(&mut guard, &self.diag);
            self.publish(&guard);
            r
        };
        self.shared.clear_txn(p);
        result
    }

    fn publish(&self, inner: &RegistryInner) {
        let mut earliest: Option<(Instant, DeadlineKind)> = None;
        let mut consider = |at: Instant, kind: DeadlineKind| match earliest {
            Some((cur, _)) if cur <= at => {}
            _ => earliest = Some((at, kind)),
        };
        for slot in inner.slots.values() {
            match &slot.state {
                SlotState::Queued | SlotState::QueuedCancelled => consider(
                    slot.accepted_at + limits::ACCEPTANCE,
                    DeadlineKind::Acceptance,
                ),
                SlotState::Admitted { admit_at, .. }
                | SlotState::Cancelled { admit_at, .. }
                | SlotState::Committed { admit_at, .. } => {
                    consider(*admit_at + limits::ADMISSION, DeadlineKind::Admission)
                }
                SlotState::Rejected { .. } => {}
            }
        }
        for d in inner.deliveries.values() {
            consider(d.commit_at + limits::DELIVERY, DeadlineKind::Delivery);
        }
        self.shared.publish_next_deadline(earliest);
    }

    pub fn counts(&self) -> Counts {
        self.transact(Participant::Runtime, |inner, _| Counts {
            requests: inner.request_count,
            admitted: inner.admitted_count(),
            deliveries: inner.deliveries.len(),
            initialized_outstanding: inner.initialized_outstanding,
            available_permits: self.admission.available_permits(),
            available_error_budget: self.error_budget.available_permits(),
            jobs: self.jobs(),
        })
    }

    // ----- reader-side operations ---------------------------------------------------------

    /// Reserves an id and queues a request. Must run before the request is published.
    pub fn accept_request(&self, id: RequestId, method: Method, bytes: usize) -> Result<(), Cause> {
        self.transact(Participant::Reader, |inner, diag| {
            if inner.slots.contains_key(&id) {
                return Err(Cause::DuplicateId);
            }
            if inner.request_count >= limits::MAX_REQUESTS {
                return Err(Cause::AdmissionOverflow);
            }
            inner.request_count += 1;
            inner.slots.insert(
                id,
                Slot {
                    method,
                    accepted_at: Instant::now(),
                    state: SlotState::Queued,
                    token: CancellationToken::new(),
                    handler_start: None,
                    handler_end: None,
                },
            );
            diag.emit(Event::RequestAccepted {
                method,
                bytes,
                requests: inner.request_count,
            });
            Ok(())
        })
    }

    /// True while any slot owns this id.
    pub fn owns(&self, id: &RequestId) -> bool {
        self.transact(Participant::Reader, |inner, _| inner.slots.contains_key(id))
    }

    /// Applies a client cancellation.
    pub fn cancel(&self, id: &RequestId) {
        self.transact(Participant::Reader, |inner, diag| {
            let Some(slot) = inner.slots.get_mut(id) else {
                diag.emit(Event::NotificationDropped {
                    reason: NotificationDrop::UnknownId,
                });
                return;
            };
            let method = slot.method;
            let state = std::mem::replace(&mut slot.state, SlotState::Queued);
            slot.state = match state {
                SlotState::Queued => {
                    slot.token.cancel();
                    diag.emit(Event::CancelAccepted { method });
                    SlotState::QueuedCancelled
                }
                SlotState::Admitted { admit_at, permit } => {
                    slot.token.cancel();
                    diag.emit(Event::CancelAccepted { method });
                    SlotState::Cancelled {
                        admit_at,
                        _permit: permit,
                    }
                }
                SlotState::Committed {
                    admit_at,
                    permit,
                    seq,
                } => {
                    diag.emit(Event::CancelLate { method });
                    SlotState::Committed {
                        admit_at,
                        permit,
                        seq,
                    }
                }
                SlotState::Rejected { seq } => {
                    diag.emit(Event::NotificationDropped {
                        reason: NotificationDrop::UnknownId,
                    });
                    SlotState::Rejected { seq }
                }
                other => other,
            };
        })
    }

    /// Reserves an error-budget permit and a delivery record for a protocol error built by the
    /// reader. A correlated error also reserves the id so a later request cannot reuse it.
    pub fn reserve_error(
        &self,
        id: Option<RequestId>,
        bytes: usize,
        code: i32,
    ) -> Result<u64, Cause> {
        self.transact(Participant::Reader, |inner, _| {
            if let Some(id) = &id
                && inner.slots.contains_key(id)
            {
                return Err(Cause::DuplicateId);
            }
            let permit = self
                .error_budget
                .clone()
                .try_acquire_owned()
                .map_err(|_| Cause::ErrorFlood)?;
            let seq = inner.next_seq();
            let now = Instant::now();
            let owner = match id {
                Some(id) => {
                    inner.slots.insert(
                        id.clone(),
                        Slot {
                            method: Method::Other,
                            accepted_at: now,
                            state: SlotState::Rejected { seq },
                            token: CancellationToken::new(),
                            handler_start: None,
                            handler_end: None,
                        },
                    );
                    Owner::Correlated(id)
                }
                None => Owner::Uncorrelated,
            };
            inner.deliveries.insert(
                seq,
                Delivery {
                    owner,
                    commit_at: now,
                    bytes,
                    _error_permit: Some(permit),
                    method: Method::Other,
                    outcome: Outcome::RpcError,
                    code,
                },
            );
            Ok(seq)
        })
    }

    /// Grants one forwarding credit for `notifications/initialized` when an initialize response
    /// has been delivered and no forwarded notification is still outstanding.
    pub fn try_forward_initialized(&self) -> Result<(), NotificationDrop> {
        self.transact(Participant::Reader, |inner, _| {
            if inner.initialized_outstanding > 0 {
                return Err(NotificationDrop::Outstanding);
            }
            if inner.initialize_committed <= inner.initialized_forwarded {
                return Err(NotificationDrop::NoCredit);
            }
            inner.initialized_forwarded += 1;
            inner.initialized_outstanding += 1;
            Ok(())
        })
    }

    pub fn revert_forward_initialized(&self) {
        self.transact(Participant::Reader, |inner, _| {
            inner.initialized_forwarded = inner.initialized_forwarded.saturating_sub(1);
            inner.initialized_outstanding = inner.initialized_outstanding.saturating_sub(1);
        })
    }

    /// Completion acknowledgment from the handler's `on_initialized`.
    pub fn initialized_done(&self) {
        self.transact(Participant::Runtime, |inner, _| {
            inner.initialized_outstanding = inner.initialized_outstanding.saturating_sub(1);
        })
    }

    // ----- adapter-side operations --------------------------------------------------------

    /// Moves a queued request to `Admitted` with the given permit, or skips it when cancelled.
    pub fn dispatch(
        &self,
        id: &RequestId,
        permit: OwnedSemaphorePermit,
    ) -> Result<Dispatch, Cause> {
        self.transact(Participant::Runtime, |inner, diag| {
            let Some(slot) = inner.slots.get_mut(id) else {
                return Err(Cause::Invariant);
            };
            let method = slot.method;
            match slot.state {
                SlotState::Queued => {
                    slot.state = SlotState::Admitted {
                        admit_at: Instant::now(),
                        permit,
                    };
                    let admitted = inner.admitted_count();
                    diag.emit(Event::RequestAdmitted { method, admitted });
                    Ok(Dispatch::Admit)
                }
                SlotState::QueuedCancelled => {
                    inner.slots.remove(id);
                    inner.request_count = inner.request_count.saturating_sub(1);
                    drop(permit);
                    diag.emit(Event::RequestSkipped { method });
                    Ok(Dispatch::Skip)
                }
                _ => Err(Cause::Invariant),
            }
        })
    }

    /// Commitment point. Transitions `Admitted` to `Committed` and creates the delivery record,
    /// or suppresses the output of a cancelled request.
    pub fn commit(&self, id: &RequestId, bytes: usize, outcome: Outcome, code: i32) -> Commit {
        self.transact(Participant::Runtime, |inner, diag| {
            let seq_candidate = inner.next_seq + 1;
            let Some(slot) = inner.slots.get_mut(id) else {
                diag.emit(Event::OutboundUnexpected);
                return Commit::Unexpected;
            };
            let method = slot.method;
            let state = std::mem::replace(&mut slot.state, SlotState::Queued);
            match state {
                SlotState::Admitted { admit_at, permit } => {
                    slot.state = SlotState::Committed {
                        admit_at,
                        permit,
                        seq: seq_candidate,
                    };
                    inner.next_seq = seq_candidate;
                    if method == Method::Initialize && outcome == Outcome::Ok {
                        // Credit for one `notifications/initialized` exists as soon as the SDK has
                        // produced the initialize response, before any client can observe it.
                        inner.initialize_committed += 1;
                    }
                    inner.deliveries.insert(
                        seq_candidate,
                        Delivery {
                            owner: Owner::Request(id.clone()),
                            commit_at: Instant::now(),
                            bytes,
                            _error_permit: None,
                            method,
                            outcome,
                            code,
                        },
                    );
                    Commit::Deliver(seq_candidate)
                }
                SlotState::Cancelled { .. } => {
                    inner.slots.remove(id);
                    inner.request_count = inner.request_count.saturating_sub(1);
                    diag.emit(Event::ResponseSuppressed { method });
                    Commit::Suppress
                }
                other => {
                    slot.state = other;
                    diag.emit(Event::OutboundUnexpected);
                    Commit::Unexpected
                }
            }
        })
    }

    /// Reserves a delivery record for an id-less error produced inside the SDK.
    pub fn reserve_uncorrelated(&self, bytes: usize, code: i32) -> Result<u64, Cause> {
        self.transact(Participant::Runtime, |inner, _| {
            let permit = self
                .error_budget
                .clone()
                .try_acquire_owned()
                .map_err(|_| Cause::ErrorFlood)?;
            let seq = inner.next_seq();
            inner.deliveries.insert(
                seq,
                Delivery {
                    owner: Owner::Uncorrelated,
                    commit_at: Instant::now(),
                    bytes,
                    _error_permit: Some(permit),
                    method: Method::Other,
                    outcome: Outcome::RpcError,
                    code,
                },
            );
            Ok(seq)
        })
    }

    // ----- handler-side operations --------------------------------------------------------

    pub fn handler_context(&self, id: &RequestId) -> Option<HandlerContext> {
        self.transact(Participant::Runtime, |inner, _| {
            let slot = inner.slots.get_mut(id)?;
            let admit_at = match &slot.state {
                SlotState::Admitted { admit_at, .. }
                | SlotState::Cancelled { admit_at, .. }
                | SlotState::Committed { admit_at, .. } => *admit_at,
                _ => return None,
            };
            slot.handler_start = Some(Instant::now());
            Some(HandlerContext {
                token: slot.token.clone(),
                admit_at,
            })
        })
    }

    pub fn handler_finished(&self, id: &RequestId) {
        self.transact(Participant::Runtime, |inner, _| {
            if let Some(slot) = inner.slots.get_mut(id) {
                slot.handler_end = Some(Instant::now());
            }
        })
    }

    // ----- writer-side operations ---------------------------------------------------------

    /// Retires a delivered frame and the slot that owned it. Returns the event to emit.
    pub fn ack(&self, seq: u64) -> Option<Event> {
        self.transact(Participant::Writer, |inner, _| {
            let delivery = inner.deliveries.remove(&seq)?;
            let now = Instant::now();
            let mut handler_ms = None;
            let mut admit_to_delivery_ms = None;
            match &delivery.owner {
                Owner::Request(id) | Owner::Correlated(id) => {
                    if let Some(slot) = inner.slots.remove(id) {
                        if slot.state.counts_as_request() {
                            inner.request_count = inner.request_count.saturating_sub(1);
                        }
                        if let (Some(s), Some(e)) = (slot.handler_start, slot.handler_end) {
                            handler_ms = Some(e.saturating_duration_since(s).as_millis() as u64);
                        }
                        if let SlotState::Committed { admit_at, .. } = slot.state {
                            admit_to_delivery_ms =
                                Some(now.saturating_duration_since(admit_at).as_millis() as u64);
                        }
                    }
                }
                Owner::Uncorrelated => {}
            }
            Some(Event::ResponseSent {
                method: delivery.method,
                outcome: delivery.outcome,
                code: delivery.code,
                bytes: delivery.bytes,
                handler_ms,
                admit_to_delivery_ms,
                seq,
            })
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Classification (independent of the SDK)
// ---------------------------------------------------------------------------------------------

/// Outcome of classifying one complete frame.
pub enum Classified {
    Request {
        msg: ClientJsonRpcMessage,
        id: RequestId,
        method: Method,
    },
    Initialized {
        msg: ClientJsonRpcMessage,
    },
    Cancelled {
        id: Option<RequestId>,
    },
    OtherNotification,
    ClientResponse {
        id: Option<RequestId>,
    },
    Invalid {
        error: ErrorData,
        id: Option<RequestId>,
        reason: RejectReason,
    },
}

const KNOWN_METHODS: &[&str] = &[
    "initialize",
    "ping",
    "server/discover",
    "tools/list",
    "tools/call",
    "prompts/list",
    "prompts/get",
    "resources/list",
    "resources/read",
    "resources/templates/list",
    "resources/subscribe",
    "resources/unsubscribe",
    "completion/complete",
    "logging/setLevel",
    "subscriptions/listen",
];

enum IdCheck {
    Absent,
    Usable(RequestId),
    TooLong,
    Unusable,
}

fn check_id(v: Option<&Value>) -> IdCheck {
    match v {
        None => IdCheck::Absent,
        Some(Value::String(s)) => {
            let serialized_len = serde_json::to_string(s)
                .map(|t| t.len())
                .unwrap_or(usize::MAX);
            if serialized_len > limits::MAX_ID_BYTES {
                IdCheck::TooLong
            } else {
                IdCheck::Usable(NumberOrString::String(Arc::from(s.as_str())))
            }
        }
        Some(Value::Number(n)) => match n.as_i64() {
            Some(i) => IdCheck::Usable(NumberOrString::Number(i)),
            None => IdCheck::Unusable,
        },
        Some(_) => IdCheck::Unusable,
    }
}

fn invalid(
    code: ErrorCode,
    message: &'static str,
    id: Option<RequestId>,
    reason: RejectReason,
) -> Classified {
    Classified::Invalid {
        error: ErrorData::new(code, message, None),
        id,
        reason,
    }
}

fn meta_protocol_version(obj: &serde_json::Map<String, Value>) -> Option<&str> {
    obj.get("params")?
        .as_object()?
        .get("_meta")?
        .as_object()?
        .get("io.modelcontextprotocol/protocolVersion")?
        .as_str()
}

fn unsupported_version(requested: &str, id: Option<RequestId>) -> Classified {
    let error = ErrorData::new(
        ErrorCode::UNSUPPORTED_PROTOCOL_VERSION,
        "Unsupported protocol version",
        Some(serde_json::json!({
            "supported": limits::SUPPORTED_PROTOCOL_VERSIONS,
            "requested": requested,
        })),
    );
    Classified::Invalid {
        error,
        id,
        reason: RejectReason::Version,
    }
}

pub fn method_of(request: &ClientRequest) -> Method {
    match request {
        ClientRequest::DiscoverRequest(_) => Method::Discover,
        ClientRequest::InitializeRequest(_) => Method::Initialize,
        ClientRequest::PingRequest(_) => Method::Ping,
        ClientRequest::ListToolsRequest(_) => Method::ToolsList,
        ClientRequest::CallToolRequest(_) => Method::ToolsCall,
        _ => Method::Other,
    }
}

/// Classifies one complete frame without the trailing newline.
pub fn classify(frame: &[u8]) -> Classified {
    let value: Value = match serde_json::from_slice(frame) {
        Ok(v) => v,
        Err(_) => {
            return invalid(
                ErrorCode::PARSE_ERROR,
                "Parse error",
                None,
                RejectReason::Parse,
            );
        }
    };
    let Some(obj) = value.as_object() else {
        return invalid(
            ErrorCode::INVALID_REQUEST,
            "Invalid Request",
            None,
            RejectReason::Envelope,
        );
    };
    let id = match check_id(obj.get("id")) {
        IdCheck::Absent => None,
        IdCheck::Usable(id) => Some(id),
        IdCheck::TooLong => {
            return invalid(
                ErrorCode::INVALID_REQUEST,
                "Invalid Request",
                None,
                RejectReason::IdLength,
            );
        }
        IdCheck::Unusable => {
            return invalid(
                ErrorCode::INVALID_REQUEST,
                "Invalid Request",
                None,
                RejectReason::Envelope,
            );
        }
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return invalid(
            ErrorCode::INVALID_REQUEST,
            "Invalid Request",
            id,
            RejectReason::Envelope,
        );
    }
    if let Some(params) = obj.get("params")
        && !params.is_object()
    {
        return invalid(
            ErrorCode::INVALID_REQUEST,
            "Invalid Request",
            id,
            RejectReason::Envelope,
        );
    }
    // Per-request version validation, independent of the SDK's lifecycle state: an unsupported
    // `_meta` version is rejected before any bootstrap exception can answer the request.
    let meta_version = meta_protocol_version(obj).map(str::to_owned);
    if let Some(v) = meta_version.as_deref()
        && !limits::SUPPORTED_PROTOCOL_VERSIONS.contains(&v)
    {
        return match id {
            Some(id) => unsupported_version(v, Some(id)),
            None => Classified::OtherNotification,
        };
    }
    let modern_meta = meta_version
        .as_deref()
        .is_some_and(|v| v >= limits::FIRST_MODERN_VERSION);
    match obj.get("method") {
        Some(Value::String(method)) => {
            // `ping` does not exist in 2026-07-28. Answer it uniformly whatever the lifecycle state.
            if method == "ping"
                && modern_meta
                && let Some(id) = id.clone()
            {
                return Classified::Invalid {
                    error: ErrorData::new(ErrorCode::METHOD_NOT_FOUND, "ping", None),
                    id: Some(id),
                    reason: RejectReason::Method,
                };
            }
            let typed = serde_json::from_value::<ClientJsonRpcMessage>(value.clone());
            match id {
                Some(id) => match typed {
                    Ok(JsonRpcMessage::Request(req)) => {
                        // The SDK parses a recognized method whose params fail its typed shape
                        // as a custom request. The contract requires Invalid Params instead.
                        if matches!(req.request, ClientRequest::CustomRequest(_))
                            && KNOWN_METHODS.contains(&method.as_str())
                        {
                            return invalid(
                                ErrorCode::INVALID_PARAMS,
                                "Invalid params",
                                Some(id),
                                RejectReason::Params,
                            );
                        }
                        let method = method_of(&req.request);
                        Classified::Request {
                            msg: JsonRpcMessage::Request(req),
                            id,
                            method,
                        }
                    }
                    _ => {
                        if KNOWN_METHODS.contains(&method.as_str()) {
                            invalid(
                                ErrorCode::INVALID_PARAMS,
                                "Invalid params",
                                Some(id),
                                RejectReason::Params,
                            )
                        } else {
                            invalid(
                                ErrorCode::INVALID_REQUEST,
                                "Invalid Request",
                                Some(id),
                                RejectReason::Envelope,
                            )
                        }
                    }
                },
                None => match typed {
                    Ok(JsonRpcMessage::Notification(n)) => {
                        enum Kind {
                            Init,
                            Cancel(Option<RequestId>),
                            Other,
                        }
                        let kind = match &n.notification {
                            ClientNotification::InitializedNotification(_) => Kind::Init,
                            ClientNotification::CancelledNotification(c) => {
                                Kind::Cancel(c.params.request_id.clone())
                            }
                            _ => Kind::Other,
                        };
                        match kind {
                            Kind::Init => Classified::Initialized {
                                msg: JsonRpcMessage::Notification(n),
                            },
                            Kind::Cancel(id) => Classified::Cancelled { id },
                            Kind::Other => Classified::OtherNotification,
                        }
                    }
                    _ => Classified::OtherNotification,
                },
            }
        }
        Some(_) => invalid(
            ErrorCode::INVALID_REQUEST,
            "Invalid Request",
            id,
            RejectReason::Envelope,
        ),
        None => {
            let has_result = obj.contains_key("result");
            let has_error = obj.contains_key("error");
            if id.is_some() && (has_result ^ has_error) {
                Classified::ClientResponse { id }
            } else {
                invalid(
                    ErrorCode::INVALID_REQUEST,
                    "Invalid Request",
                    id,
                    RejectReason::Envelope,
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Threads and adapter
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Running,
    Eof,
    Fatal,
}

struct Accepted {
    id: RequestId,
    msg: ClientJsonRpcMessage,
}

struct OutFrame {
    seq: u64,
    bytes: Vec<u8>,
}

enum FrameRead {
    Line,
    Oversized { bytes_seen: usize },
    Eof,
    Io,
}

fn read_frame<R: Read>(reader: &mut BufReader<R>, buf: &mut Vec<u8>) -> FrameRead {
    buf.clear();
    loop {
        let (take, newline) = {
            let avail = match reader.fill_buf() {
                Ok(a) => a,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return FrameRead::Io,
            };
            if avail.is_empty() {
                return FrameRead::Eof;
            }
            match avail.iter().position(|&b| b == b'\n') {
                Some(pos) => (pos + 1, true),
                None => (avail.len(), false),
            }
        };
        if buf.len() + take > limits::MAX_FRAME_BYTES {
            return FrameRead::Oversized {
                bytes_seen: buf.len() + take,
            };
        }
        let avail = reader.fill_buf().unwrap_or(&[]);
        buf.extend_from_slice(&avail[..take]);
        reader.consume(take);
        if newline {
            return FrameRead::Line;
        }
    }
}

fn error_frame(error: ErrorData, id: Option<RequestId>) -> Vec<u8> {
    let msg: ServerJsonRpcMessage = JsonRpcMessage::error(error, id);
    let mut bytes = serde_json::to_vec(&msg).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

fn reader_loop<R: Read>(
    input: R,
    registry: Arc<Registry>,
    req_tx: mpsc::Sender<Accepted>,
    init_tx: mpsc::Sender<ClientJsonRpcMessage>,
    out_tx: mpsc::Sender<OutFrame>,
    control: watch::Sender<Control>,
) {
    let diag = registry.diag().clone();
    let shared = registry.shared().clone();
    let mut reader = BufReader::with_capacity(8192, input);
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    loop {
        if shared.is_fatal() {
            return;
        }
        match read_frame(&mut reader, &mut buf) {
            FrameRead::Line => {}
            FrameRead::Oversized { bytes_seen } => {
                diag.emit(Event::FrameOversized { bytes_seen });
                registry.fatal(Cause::FrameOversized);
                let _ = control.send(Control::Fatal);
                return;
            }
            FrameRead::Eof => {
                shared.trigger_orderly(Trigger::Eof);
                let _ = control.send(Control::Eof);
                return;
            }
            FrameRead::Io => {
                registry.fatal(Cause::ReadError);
                let _ = control.send(Control::Fatal);
                return;
            }
        }
        let mut line: &[u8] = &buf;
        if let Some(stripped) = line.strip_suffix(b"\n") {
            line = stripped;
        }
        if let Some(stripped) = line.strip_suffix(b"\r") {
            line = stripped;
        }
        if line.is_empty() {
            continue;
        }
        let bytes = buf.len();
        match classify(line) {
            Classified::Request { msg, id, method } => {
                if let Err(cause) = registry.accept_request(id.clone(), method, bytes) {
                    registry.fatal(cause);
                    let _ = control.send(Control::Fatal);
                    return;
                }
                if req_tx.try_send(Accepted { id, msg }).is_err() {
                    registry.fatal(Cause::Invariant);
                    let _ = control.send(Control::Fatal);
                    return;
                }
            }
            Classified::Initialized { msg } => match registry.try_forward_initialized() {
                Ok(()) => {
                    if init_tx.try_send(msg).is_err() {
                        registry.revert_forward_initialized();
                        diag.emit(Event::NotificationDropped {
                            reason: NotificationDrop::Outstanding,
                        });
                    }
                }
                Err(reason) => diag.emit(Event::NotificationDropped { reason }),
            },
            Classified::Cancelled { id } => match id {
                Some(id) => registry.cancel(&id),
                None => diag.emit(Event::NotificationDropped {
                    reason: NotificationDrop::Malformed,
                }),
            },
            Classified::OtherNotification => {
                diag.emit(Event::NotificationDropped {
                    reason: NotificationDrop::Unsupported,
                });
            }
            Classified::ClientResponse { id } => {
                if let Some(id) = &id
                    && registry.owns(id)
                {
                    registry.fatal(Cause::DuplicateId);
                    let _ = control.send(Control::Fatal);
                    return;
                }
                diag.emit(Event::NotificationDropped {
                    reason: NotificationDrop::ClientResponse,
                });
            }
            Classified::Invalid { error, id, reason } => {
                if let Some(id) = &id
                    && registry.owns(id)
                {
                    registry.fatal(Cause::DuplicateId);
                    let _ = control.send(Control::Fatal);
                    return;
                }
                let code = error.code.0;
                let frame = error_frame(error, id.clone());
                // Adapter-generated errors are bound by the same per-frame outbound limit as
                // SDK responses: an oversized frame is never published and the process ends
                // with the protocol-violation code, exactly as for an oversized SDK frame.
                if frame.len() > limits::MAX_OUTBOUND_FRAME_BYTES {
                    diag.emit(Event::OutboundOversized { bytes: frame.len() });
                    registry.fatal(Cause::OutboundTooLarge);
                    let _ = control.send(Control::Fatal);
                    return;
                }
                match registry.reserve_error(id, frame.len(), code) {
                    Ok(seq) => {
                        diag.emit(Event::FrameRejected { reason, bytes });
                        if out_tx.try_send(OutFrame { seq, bytes: frame }).is_err() {
                            registry.fatal(Cause::Invariant);
                            let _ = control.send(Control::Fatal);
                            return;
                        }
                    }
                    Err(cause) => {
                        registry.fatal(cause);
                        let _ = control.send(Control::Fatal);
                        return;
                    }
                }
            }
        }
    }
}

fn writer_loop<W: Write>(mut out: W, mut rx: mpsc::Receiver<OutFrame>, registry: Arc<Registry>) {
    let diag = registry.diag().clone();
    while let Some(frame) = rx.blocking_recv() {
        if registry.shared().is_fatal() {
            return;
        }
        diag.emit(Event::WriteBegan {
            seq: frame.seq,
            bytes: frame.bytes.len(),
        });
        if out
            .write_all(&frame.bytes)
            .and_then(|_| out.flush())
            .is_err()
        {
            registry.fatal(Cause::WriteError);
            return;
        }
        if let Some(event) = registry.ack(frame.seq) {
            diag.emit(event);
        }
    }
}

/// Transport error type. Failures are latched as fatal causes; the SDK only ever sees `Ok`.
#[derive(Debug)]
pub struct TransportError(&'static str);

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for TransportError {}

/// The bounded transport handed to the SDK.
pub struct BoundedStdio {
    registry: Arc<Registry>,
    req_rx: mpsc::Receiver<Accepted>,
    init_rx: mpsc::Receiver<ClientJsonRpcMessage>,
    out_tx: mpsc::Sender<OutFrame>,
    control: watch::Receiver<Control>,
    ct: CancellationToken,
    /// Keeps the control channel open after the reader thread exits, so `changed()` stays pending
    /// instead of erroring and ending `receive` before the handoff channel is drained.
    _control_tx: watch::Sender<Control>,
    /// A request taken from the handoff channel that is waiting for an admission permit. Stored
    /// in `self` so a dropped `receive` future cannot lose it.
    staged: Option<Accepted>,
}

impl BoundedStdio {
    /// Spawns reader and writer threads over arbitrary streams. Tests pass pipes.
    pub fn spawn<R, W>(input: R, output: W, registry: Arc<Registry>) -> BoundedStdio
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
    {
        let (req_tx, req_rx) = mpsc::channel::<Accepted>(limits::HANDOFF_SLOTS);
        let (init_tx, init_rx) = mpsc::channel::<ClientJsonRpcMessage>(1);
        let (out_tx, out_rx) = mpsc::channel::<OutFrame>(limits::OUTBOUND_SLOTS);
        let (control_tx, control_rx) = watch::channel(Control::Running);
        let control_keepalive = control_tx.clone();
        let ct = registry.shared().token();
        {
            let registry = registry.clone();
            let out_tx = out_tx.clone();
            std::thread::Builder::new()
                .name("token-saver-reader".into())
                .spawn(move || reader_loop(input, registry, req_tx, init_tx, out_tx, control_tx))
                .expect("spawn reader thread");
        }
        {
            let registry = registry.clone();
            std::thread::Builder::new()
                .name("token-saver-writer".into())
                .spawn(move || writer_loop(output, out_rx, registry))
                .expect("spawn writer thread");
        }
        BoundedStdio {
            registry,
            req_rx,
            init_rx,
            out_tx,
            control: control_rx,
            ct,
            _control_tx: control_keepalive,
            staged: None,
        }
    }

    /// Production constructor over the process's stdin and stdout.
    pub fn stdio(registry: Arc<Registry>) -> BoundedStdio {
        Self::spawn(std::io::stdin(), std::io::stdout(), registry)
    }

    fn outcome_of(result: &ServerResult) -> Outcome {
        match result {
            ServerResult::CallToolResult(r) if r.is_error == Some(true) => Outcome::ToolError,
            _ => Outcome::Ok,
        }
    }

    fn send_sync(&self, item: TxJsonRpcMessage<RoleServer>) -> Result<(), TransportError> {
        let (id, outcome, code) = match &item {
            JsonRpcMessage::Response(r) => (Some(r.id.clone()), Self::outcome_of(&r.result), 0),
            JsonRpcMessage::Error(e) => (e.id.clone(), Outcome::RpcError, e.error.code.0),
            JsonRpcMessage::Request(_) | JsonRpcMessage::Notification(_) => {
                self.registry.diag().emit(Event::OutboundUnexpected);
                return Ok(());
            }
        };
        if self.registry.shared().is_fatal() {
            return Ok(());
        }
        let mut bytes = serde_json::to_vec(&item).map_err(|_| TransportError("serialize"))?;
        bytes.push(b'\n');
        if bytes.len() > limits::MAX_OUTBOUND_FRAME_BYTES {
            self.registry
                .diag()
                .emit(Event::OutboundOversized { bytes: bytes.len() });
            self.registry.fatal(Cause::OutboundTooLarge);
            return Ok(());
        }
        let seq = match id {
            Some(id) => match self.registry.commit(&id, bytes.len(), outcome, code) {
                Commit::Deliver(seq) => seq,
                Commit::Suppress | Commit::Unexpected => return Ok(()),
            },
            None => match self.registry.reserve_uncorrelated(bytes.len(), code) {
                Ok(seq) => seq,
                Err(cause) => {
                    self.registry.fatal(cause);
                    return Ok(());
                }
            },
        };
        if self.out_tx.try_send(OutFrame { seq, bytes }).is_err() {
            self.registry.fatal(Cause::Invariant);
        }
        Ok(())
    }

    /// Fatal conditions stop everything at once. EOF does not: requests accepted before EOF are
    /// still dispatched, and `receive` returns `None` only when the reader has exited and the
    /// handoff channel is empty.
    fn stopped(&self) -> bool {
        self.registry.shared().is_fatal() || matches!(*self.control.borrow(), Control::Fatal)
    }

    /// Waits until every delivery record has been acknowledged by the writer and every workspace
    /// job has ended, or a fatal cause is latched, or the shutdown budget has expired (the
    /// supervisor then forces exit 1).
    async fn drain_deliveries(&self) {
        loop {
            let shared = self.registry.shared();
            if shared.is_fatal() {
                return;
            }
            let counts = self.registry.counts();
            if counts.deliveries == 0 && counts.jobs == 0 {
                return;
            }
            let deadline = shared.shutdown_deadline_nanos();
            if deadline != 0 && shared.now_nanos() >= deadline {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
}

impl Transport<RoleServer> for BoundedStdio {
    type Error = TransportError;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send + 'static {
        let result = self.send_sync(item);
        std::future::ready(result)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            if self.stopped() {
                return None;
            }
            if self.staged.is_some() {
                // A request is waiting for a permit. Admission permits are acquired only when a
                // request exists, so idle operation holds none.
                let admission = self.registry.admission();
                tokio::select! {
                    biased;
                    _ = self.ct.cancelled() => return None,
                    changed = self.control.changed() => {
                        if changed.is_err() { return None; }
                        continue;
                    }
                    Some(msg) = self.init_rx.recv() => return Some(msg),
                    permit = admission.acquire_owned() => {
                        let Ok(permit) = permit else { return None };
                        let accepted = self.staged.take().expect("staged request present");
                        match self.registry.dispatch(&accepted.id, permit) {
                            Ok(Dispatch::Admit) => return Some(accepted.msg),
                            Ok(Dispatch::Skip) => continue,
                            Err(cause) => {
                                self.registry.fatal(cause);
                                return None;
                            }
                        }
                    }
                    else => return None,
                }
            }
            tokio::select! {
                biased;
                _ = self.ct.cancelled() => return None,
                changed = self.control.changed() => {
                    if changed.is_err() { return None; }
                    continue;
                }
                Some(msg) = self.init_rx.recv() => return Some(msg),
                accepted = self.req_rx.recv() => {
                    let accepted = accepted?;
                    match self.registry.admission().try_acquire_owned() {
                        Ok(permit) => match self.registry.dispatch(&accepted.id, permit) {
                            Ok(Dispatch::Admit) => return Some(accepted.msg),
                            Ok(Dispatch::Skip) => continue,
                            Err(cause) => {
                                self.registry.fatal(cause);
                                return None;
                            }
                        },
                        Err(_) => {
                            self.staged = Some(accepted);
                            continue;
                        }
                    }
                }
                else => return None,
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.drain_deliveries().await;
        Ok(())
    }
}
