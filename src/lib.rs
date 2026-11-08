//! token-saver: a minimal MCP server over stdio.
//!
//! Module map:
//! - [`diag`]: allowlisted, bounded diagnostics on stderr.
//! - [`supervisor`]: process watchdog running outside the async runtime.
//! - [`transport`]: bounded stdio transport, request registry, admission and delivery accounting.
//! - [`probe`]: the `connection_probe` tool and the `ServerHandler` implementation.
//! - [`workspace`]: the opt-in Workspace Reading Pack (`--root`): three read-only tools served
//!   by an owned worker pool over a root directory descriptor (Milestone 1).

#![forbid(unsafe_code)]
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod diag;
pub mod probe;
pub mod supervisor;
pub mod transport;
pub mod workspace;

/// Fixed limits. None of these can be raised at runtime.
pub mod limits {
    use std::time::Duration;

    /// Maximum inbound frame size, including the newline delimiter.
    pub const MAX_FRAME_BYTES: usize = 65_536;
    /// Maximum serialized JSON-RPC id accepted (string form including quotes).
    pub const MAX_ID_BYTES: usize = 256;
    /// Maximum requests admitted to the SDK at once.
    pub const MAX_ADMITTED: usize = 8;
    /// Maximum unretired requests: admitted plus one pending.
    pub const MAX_REQUESTS: usize = 9;
    /// Maximum outstanding adapter-generated error frames.
    pub const ERROR_BUDGET: usize = 4;
    /// Maximum serialized outbound frame, including newline.
    pub const MAX_OUTBOUND_FRAME_BYTES: usize = 16_384;
    /// Outbound channel capacity: admissions plus error budget.
    pub const OUTBOUND_SLOTS: usize = MAX_ADMITTED + ERROR_BUDGET;
    /// Handoff channel capacity: equals the request capacity so it never limits ingress.
    pub const HANDOFF_SLOTS: usize = MAX_REQUESTS;
    /// Maximum serialized `arguments` object accepted by any tool.
    pub const MAX_ARGUMENTS_BYTES: usize = 4_096;
    /// Maximum serialized probe `result` object.
    pub const MAX_RESULT_BYTES: usize = 1_024;

    /// Cooperative processing deadline from admission.
    pub const PROCESSING: Duration = Duration::from_secs(5);
    /// Delivery deadline from commitment, covering queue wait, write, and flush.
    pub const DELIVERY: Duration = Duration::from_secs(5);
    /// Maximum unresolved admission from admission.
    pub const ADMISSION: Duration = Duration::from_secs(10);
    /// Maximum time a request may wait for admission after acceptance.
    pub const ACCEPTANCE: Duration = Duration::from_secs(10);
    /// Maximum registry transaction duration, armed before lock acquisition.
    pub const TRANSACTION: Duration = Duration::from_secs(1);
    /// Maximum startup interval before the reader, writer, and runtime are running.
    pub const STARTUP: Duration = Duration::from_secs(3);
    /// Maximum shutdown interval, orderly or fatal.
    pub const SHUTDOWN: Duration = Duration::from_secs(3);
    /// Supervisor polling interval. Adds up to this much detection latency.
    pub const TICK: Duration = Duration::from_millis(100);
    /// Diagnostics queue depth.
    pub const DIAG_QUEUE: usize = 256;
    /// Diagnostics rate limit per second.
    pub const DIAG_PER_SECOND: u32 = 64;
    /// Advertised protocol versions. The adapter rejects any other `_meta` version itself.
    pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2026-07-28"];
    /// First protocol version without the `initialize` handshake.
    pub const FIRST_MODERN_VERSION: &str = "2026-07-28";

    // ----- Workspace Reading Pack (Milestone 1) ---------------------------------------------

    /// Worker threads created when `--root` is given.
    pub const WORKERS: usize = 4;
    /// Job slots: a handler holds one from enqueue until the job has released every resource.
    pub const JOB_SLOTS: usize = 8;
    /// Budget for one blocking step (one system call), measured from the step's start.
    pub const STEP: Duration = Duration::from_secs(2);
    /// Grace after the job's deadline before the supervisor latches `worker_stall`.
    pub const JOB_GRACE: Duration = Duration::from_millis(2_500);
    /// Maximum time a handler waits for a job slot before answering `busy`.
    pub const SLOT_WAIT: Duration = Duration::from_secs(2);
    /// Maximum serialized workspace tool `result` object.
    pub const MAX_WORKSPACE_RESULT_BYTES: usize = 12_288;
    /// Path argument and generated-path limits.
    pub const MAX_PATH_BYTES: usize = 1_024;
    pub const MAX_PATH_COMPONENTS: usize = 64;
    pub const MAX_COMPONENT_BYTES: usize = 255;
    /// One read step.
    pub const READ_STEP_BYTES: usize = 65_536;
    /// Directory entries pulled per step.
    pub const DIR_BATCH: usize = 256;
    /// Entries read from any one directory; the rest is unread and the directory is `truncated`.
    pub const DIR_ENTRY_LIMIT: usize = 5_000;
    /// Raw directory entries read by one outline job.
    pub const OUTLINE_RAW_ENTRY_LIMIT: usize = 5_000;
    /// Search bounds.
    pub const SEARCH_FILE_BYTES: usize = 1_048_576;
    pub const SEARCH_MAX_FILES: usize = 2_000;
    pub const SEARCH_MAX_DIRS: usize = 4_096;
    pub const SEARCH_MAX_ENTRIES: usize = 20_000;
    pub const SEARCH_SCAN_BUDGET_BYTES: usize = 64 * 1_048_576;
    pub const SEARCH_TIME_BUDGET: Duration = Duration::from_secs(2);
    /// Binary detection window.
    pub const BINARY_SNIFF_BYTES: usize = 8_192;
    /// Line cutting.
    pub const SLICE_LINE_CHARS: usize = 400;
    pub const SEARCH_LINE_CHARS: usize = 200;
    /// Bytes read past a cut line to count its remaining characters.
    pub const LINE_LOOKAHEAD_BYTES: usize = 1_048_576;
    /// Files up to this size have `total_lines` counted.
    pub const TOTAL_LINES_MAX_FILE_BYTES: u64 = 1_048_576;
}
