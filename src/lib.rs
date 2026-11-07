//! token-saver: a minimal MCP connectivity-probe server over stdio.
//!
//! Module map (Milestone 0):
//! - [`diag`]: allowlisted, bounded diagnostics on stderr.
//! - [`supervisor`]: process watchdog running outside the async runtime.
//! - [`transport`]: bounded stdio transport, request registry, admission and delivery accounting.
//! - [`probe`]: the `connection_probe` tool and `ServerHandler` implementation.

#![forbid(unsafe_code)]
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod diag;
pub mod probe;
pub mod supervisor;
pub mod transport;

/// Fixed Milestone 0 limits. None of these can be raised at runtime.
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
    /// Maximum serialized `arguments` object accepted by the probe.
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
}
