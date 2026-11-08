//! evidence-relay: development-only stdio relay that records allowlisted MCP evidence fields.
//!
//! Usage:
//!   evidence-relay --expect-nonce <N> --out <DIR> -- <server> [args...]
//!   evidence-relay --inspect-launch --out <DIR>
//!
//! The relay sits between a client and the server. It forwards bytes unchanged in both
//! directions and persists one record per frame containing only allowlisted fields. It takes an
//! exclusive lock on the evidence directory, reserves a storage budget from actual file sizes,
//! writes an incomplete manifest before spawning the child, and marks the run complete only after
//! an orderly end with every record flushed and recounted.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MAX_FRAME: usize = 65_536;
const MAX_RUN_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DIR_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DIR_FILES: usize = 64;
const CHILD_GRACE: Duration = Duration::from_secs(1);
const EOF_WAIT: Duration = Duration::from_secs(3);
/// Maximum time to wait for a forwarding thread after the child is gone.
const THREAD_DRAIN: Duration = Duration::from_secs(1);
/// Maximum request ids kept for response correlation.
const MAX_PENDING: usize = 1024;
/// Window in which stdin must stay idle for an open input to count as quiescent at the boundary.
const QUIESCENCE_MS: u64 = 150;
/// Time given to the inbound thread to observe EOF after a hang-up at the boundary.
const EOF_GRACE_MS: u64 = 150;
/// Time given to frames admitted before the seal to publish before the record set closes.
const IN_FLIGHT_DRAIN_MS: u64 = 150;

/// Terminal state of a forwarding thread. Anything other than a clean end makes the run incomplete.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Outcome {
    Running = 0,
    CleanEof = 1,
    FramingError = 2,
    ReadError = 3,
    ForwardError = 4,
    /// Input still open at the capture boundary, with no data pending on stdin.
    Quiescent = 5,
}

impl Outcome {
    fn from_u8(v: u8) -> Outcome {
        match v {
            1 => Outcome::CleanEof,
            2 => Outcome::FramingError,
            3 => Outcome::ReadError,
            4 => Outcome::ForwardError,
            5 => Outcome::Quiescent,
            _ => Outcome::Running,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Running => "running",
            Outcome::CleanEof => "clean_eof",
            Outcome::FramingError => "framing_error",
            Outcome::ReadError => "read_error",
            Outcome::ForwardError => "forward_error",
            Outcome::Quiescent => "quiescent",
        }
    }
}

/// Bounded request-id correlation. Entries retire on result, error, or cancellation; the oldest
/// is evicted when the bound is reached so memory cannot grow with unanswered requests.
#[derive(Default)]
struct Pending {
    map: HashMap<String, &'static str>,
    order: std::collections::VecDeque<String>,
}

impl Pending {
    fn insert(&mut self, key: String, kind: &'static str) {
        if !self.map.contains_key(&key) {
            if self.order.len() >= MAX_PENDING
                && let Some(old) = self.order.pop_front()
            {
                self.map.remove(&old);
            }
            self.order.push_back(key.clone());
        }
        self.map.insert(key, kind);
    }
    fn remove(&mut self, key: &str) -> Option<&'static str> {
        let v = self.map.remove(key);
        if v.is_some() {
            self.order.retain(|k| k != key);
        }
        v
    }
    fn len(&self) -> usize {
        self.map.len()
    }
}

fn wait_flag(flag: &AtomicU8, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if flag.load(Ordering::SeqCst) != Outcome::Running as u8 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    flag.load(Ordering::SeqCst) != Outcome::Running as u8
}

static GOT_SIGNAL: AtomicBool = AtomicBool::new(false);
static SIGNAL_NO: AtomicUsize = AtomicUsize::new(0);

/// Test hook (development relay only): when this variable names a directory, the inbound thread
/// pauses after admitting a frame whose bytes contain `PAUSE`, creates `<dir>/waiting`, and resumes
/// once `<dir>/release` exists. It lets a test place a frame between admission and publication
/// while finalization runs. Never set outside tests.
const TEST_PAUSE_DIR: &str = "EVIDENCE_RELAY_TEST_PAUSE_DIR";

fn test_pause_point(buf: &[u8]) {
    let Ok(dir) = std::env::var(TEST_PAUSE_DIR) else {
        return;
    };
    if !buf.windows(5).any(|w| w == b"PAUSE") {
        return;
    }
    let dir = PathBuf::from(dir);
    let _ = fs::write(dir.join("waiting"), b"");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !dir.join("release").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// State of stdin at the capture boundary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StdinState {
    /// Nothing arrived within the window.
    Idle,
    /// The client closed its end; the inbound thread is about to observe EOF.
    HangUp,
    /// Data is pending that was not captured.
    Data,
    Error,
}

fn stdin_state(timeout_ms: u64) -> StdinState {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let mut pfd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll on a valid descriptor with a correctly sized array of one entry.
        let n = unsafe { libc::poll(&mut pfd, 1, remaining.as_millis() as i32) };
        if n < 0 {
            // A second signal from the client (SIGTERM after SIGINT) interrupts the poll: retry
            // until the window ends.
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                && Instant::now() < deadline
            {
                continue;
            }
            return StdinState::Error;
        }
        if n == 0 {
            return StdinState::Idle;
        }
        if (pfd.revents & libc::POLLHUP) != 0 {
            return StdinState::HangUp;
        }
        if (pfd.revents & libc::POLLIN) != 0 {
            return StdinState::Data;
        }
        return StdinState::Idle;
    }
}

extern "C" fn on_signal(sig: libc::c_int) {
    SIGNAL_NO.store(sig as usize, Ordering::SeqCst);
    GOT_SIGNAL.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    // SAFETY: installing a handler that only stores to atomics.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
    }
}

struct Args {
    expect_nonce: Option<String>,
    out: PathBuf,
    inspect_launch: bool,
    server: Vec<String>,
}

fn usage() -> ! {
    eprintln!(
        "usage: evidence-relay --expect-nonce <N> --out <DIR> -- <server> [args...]\n       evidence-relay --inspect-launch --out <DIR>"
    );
    std::process::exit(2)
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut expect_nonce = None;
    let mut out = None;
    let mut inspect_launch = false;
    let mut server = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--expect-nonce" => {
                expect_nonce = argv.get(i + 1).cloned();
                i += 2;
            }
            "--out" => {
                out = argv.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--inspect-launch" => {
                inspect_launch = true;
                i += 1;
            }
            "--" => {
                server = argv[i + 1..].to_vec();
                break;
            }
            _ => usage(),
        }
    }
    let Some(out) = out else { usage() };
    if !inspect_launch && server.is_empty() {
        usage();
    }
    Args {
        expect_nonce,
        out,
        inspect_launch,
        server,
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn sha_prefix(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

const KNOWN_METHODS: &[&str] = &[
    "initialize",
    "notifications/initialized",
    "notifications/cancelled",
    "ping",
    "server/discover",
    "tools/list",
    "tools/call",
];
const KNOWN_VERSIONS: &[&str] = &[
    "2024-11-05",
    "2025-03-26",
    "2025-06-18",
    "2025-11-25",
    "2026-07-28",
];

/// Tool names recorded verbatim; anything else is `other`.
const KNOWN_TOOLS: &[&str] = &[
    "connection_probe",
    "workspace_outline",
    "file_slice",
    "text_search",
];

fn is_nonce_shape(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Canonical JSON (docs/evidence/README.md): keys sorted by code point, no whitespace, integers in
/// decimal, `true`/`false`/`null`, strings escaping only `"`, `\`, and control characters (short
/// forms for U+0008, U+0009, U+000A, U+000C, U+000D; `\u00xx` lowercase otherwise), everything
/// else as raw UTF-8. Returns `None` when a number is not an integer.
pub fn canonical_json(v: &Value, out: &mut String) -> Option<()> {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            let integer = n
                .as_i64()
                .map(|i| i.to_string())
                .or_else(|| n.as_u64().map(|u| u.to_string()))?;
            out.push_str(&integer);
        }
        Value::String(s) => canonical_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_json(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            // UTF-8 byte order equals code point order.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_string(k, out);
                out.push(':');
                canonical_json(&map[*k], out)?;
            }
            out.push('}');
        }
    }
    Some(())
}

fn canonical_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `(args_sha256, args_bytes)` of a `tools/call` arguments value, or `None` when not canonical.
pub fn args_digest(arguments: &Value) -> Option<(String, usize)> {
    let mut canonical = String::new();
    canonical_json(arguments, &mut canonical)?;
    Some((sha_prefix(canonical.as_bytes()), canonical.len()))
}

/// Builds one allowlisted record for a frame. Never persists raw text.
fn record(
    dir: &str,
    bytes: &[u8],
    elapsed_ms: u64,
    expect_nonce: Option<&str>,
    pending: &mut Pending,
) -> Value {
    let mut rec = json!({
        "elapsed_ms": elapsed_ms,
        "unix_ms": unix_ms(),
        "dir": dir,
        "bytes": bytes.len(),
    });
    let obj = rec.as_object_mut().expect("object");
    let value: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => {
            obj.insert("kind".into(), json!("unparsable"));
            return rec;
        }
    };
    let Some(map) = value.as_object() else {
        obj.insert("kind".into(), json!("non_object"));
        return rec;
    };
    let id_key = map
        .get("id")
        .map(|id| sha_prefix(id.to_string().as_bytes()));
    if let Some(k) = &id_key {
        obj.insert("id_sha256".into(), json!(k));
    }
    match map.get("method").and_then(Value::as_str) {
        Some(m) => {
            let known = KNOWN_METHODS.contains(&m);
            obj.insert(
                "kind".into(),
                json!(if map.contains_key("id") {
                    "request"
                } else {
                    "notification"
                }),
            );
            obj.insert("method".into(), json!(if known { m } else { "other" }));
            let params = map.get("params").and_then(Value::as_object);
            if let Some(p) = params {
                if let Some(v) = p.get("protocolVersion").and_then(Value::as_str) {
                    obj.insert(
                        "protocol_version".into(),
                        json!(if KNOWN_VERSIONS.contains(&v) {
                            v
                        } else {
                            "other"
                        }),
                    );
                }
                if let Some(v) = p
                    .get("_meta")
                    .and_then(Value::as_object)
                    .and_then(|m| m.get("io.modelcontextprotocol/protocolVersion"))
                    .and_then(Value::as_str)
                {
                    obj.insert(
                        "meta_protocol_version".into(),
                        json!(if KNOWN_VERSIONS.contains(&v) {
                            v
                        } else {
                            "other"
                        }),
                    );
                }
                if m == "notifications/cancelled"
                    && let Some(rid) = p.get("requestId")
                {
                    pending.remove(&sha_prefix(rid.to_string().as_bytes()));
                }
                if m == "tools/call" {
                    let tool = p.get("name").and_then(Value::as_str).unwrap_or("");
                    obj.insert(
                        "tool".into(),
                        json!(if KNOWN_TOOLS.contains(&tool) {
                            tool
                        } else {
                            "other"
                        }),
                    );
                    let arguments = p.get("arguments").cloned().unwrap_or(Value::Null);
                    match args_digest(&arguments) {
                        Some((digest, bytes)) => {
                            obj.insert("args_sha256".into(), json!(digest));
                            obj.insert("args_bytes".into(), json!(bytes));
                        }
                        None => {
                            obj.insert("args_sha256".into(), Value::Null);
                            obj.insert("args_canonical".into(), json!(false));
                        }
                    }
                    let nonce = p
                        .get("arguments")
                        .and_then(Value::as_object)
                        .and_then(|a| a.get("nonce"))
                        .and_then(Value::as_str);
                    match (nonce, expect_nonce) {
                        (Some(n), Some(e)) if n == e => {
                            obj.insert("nonce_match".into(), json!(true));
                        }
                        (Some(n), _) => {
                            obj.insert("nonce_match".into(), json!(false));
                            obj.insert("nonce_sha256".into(), json!(sha_prefix(n.as_bytes())));
                        }
                        (None, _) => {
                            obj.insert("nonce_match".into(), json!(false));
                        }
                    }
                }
                if let Some(k) = &id_key
                    && map.contains_key("id")
                {
                    pending.insert(
                        k.clone(),
                        if m == "tools/call" {
                            "tools/call"
                        } else {
                            "other"
                        },
                    );
                }
            } else if let Some(k) = &id_key {
                pending.insert(
                    k.clone(),
                    if m == "tools/call" {
                        "tools/call"
                    } else {
                        "other"
                    },
                );
            }
        }
        None => {
            if let Some(err) = map.get("error").and_then(Value::as_object) {
                obj.insert("kind".into(), json!("error"));
                let for_call =
                    id_key.as_ref().and_then(|k| pending.remove(k)) == Some("tools/call");
                if for_call {
                    obj.insert("result_sha256".into(), Value::Null);
                }
                if let Some(code) = err.get("code").and_then(Value::as_i64) {
                    obj.insert("error_code".into(), json!(code));
                }
            } else if let Some(result) = map.get("result") {
                obj.insert("kind".into(), json!("response"));
                let for_call =
                    id_key.as_ref().and_then(|k| pending.remove(k)) == Some("tools/call");
                if for_call && let Some(r) = result.as_object() {
                    let is_error = r.get("isError") == Some(&Value::Bool(true));
                    obj.insert("is_error".into(), json!(is_error));
                    if is_error {
                        obj.insert("tool_error".into(), json!(true));
                    }
                    let text = r
                        .get("content")
                        .and_then(Value::as_array)
                        .and_then(|c| c.first())
                        .and_then(|b| b.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    obj.insert("result_sha256".into(), json!(sha_prefix(text.as_bytes())));
                    obj.insert("result_bytes".into(), json!(text.len()));
                    if let Some(sc) = r.get("structuredContent").and_then(Value::as_object) {
                        if let Some(iid) = sc.get("instance_id").and_then(Value::as_str)
                            && iid.len() == 32
                            && iid
                                .bytes()
                                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                        {
                            obj.insert("instance_id".into(), json!(iid));
                        }
                        if sc.get("status") == Some(&Value::String("ok".into())) {
                            obj.insert("status".into(), json!("ok"));
                        }
                        match (sc.get("nonce").and_then(Value::as_str), expect_nonce) {
                            (Some(n), Some(e)) if n == e && is_nonce_shape(n) => {
                                obj.insert("nonce_match".into(), json!(true));
                            }
                            (Some(n), _) => {
                                obj.insert("nonce_match".into(), json!(false));
                                obj.insert("nonce_sha256".into(), json!(sha_prefix(n.as_bytes())));
                            }
                            _ => {}
                        }
                    }
                }
                if let Some(r) = result.as_object() {
                    if let Some(v) = r.get("protocolVersion").and_then(Value::as_str) {
                        obj.insert(
                            "protocol_version".into(),
                            json!(if KNOWN_VERSIONS.contains(&v) {
                                v
                            } else {
                                "other"
                            }),
                        );
                    }
                    if let Some(sv) = r.get("supportedVersions").and_then(Value::as_array) {
                        let versions: Vec<&str> = sv
                            .iter()
                            .filter_map(Value::as_str)
                            .map(|v| {
                                if KNOWN_VERSIONS.contains(&v) {
                                    v
                                } else {
                                    "other"
                                }
                            })
                            .collect();
                        obj.insert("supported_versions".into(), json!(versions));
                    }
                }
            } else {
                obj.insert("kind".into(), json!("invalid"));
            }
        }
    }
    rec
}

/// Capture phases. `Open`: frames are admitted and published. `Sealed`: no new frame is admitted
/// (late), but frames admitted earlier may still publish while finalization drains them.
/// `Closed`: nothing may be written; an in-flight frame that reaches publication is dropped and
/// counted. Every transition and every admission or publication happens under the recorder lock,
/// so the record set is fixed before it is flushed and recounted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Open,
    Sealed,
    Closed,
}

struct Recorder {
    file: BufWriter<File>,
    lines: u64,
    bytes: u64,
    dropped: u64,
    write_error: bool,
    limit_reached: bool,
    phase: Phase,
    /// Frames admitted but not yet published.
    in_flight: u64,
    /// Frames that arrived after the boundary: never recorded or forwarded.
    late: u64,
    /// Frames admitted before the boundary whose publication came after the record set closed.
    dropped_in_flight: u64,
}

impl Recorder {
    fn new(file: File) -> Recorder {
        Recorder {
            file: BufWriter::new(file),
            lines: 0,
            bytes: 0,
            dropped: 0,
            write_error: false,
            limit_reached: false,
            phase: Phase::Open,
            in_flight: 0,
            late: 0,
            dropped_in_flight: 0,
        }
    }

    /// Admits a frame that was just read. False means the capture boundary has passed: the frame
    /// is counted as late and must be neither recorded nor forwarded.
    fn admit(&mut self) -> bool {
        if self.phase != Phase::Open {
            self.late += 1;
            return false;
        }
        self.in_flight += 1;
        true
    }

    /// Publishes the record of an admitted frame (`None` for a frame with no record, such as an
    /// empty line). After `Closed`, the frame is dropped and counted instead of written.
    fn publish(&mut self, rec: Option<&Value>) {
        if self.phase == Phase::Closed {
            self.dropped_in_flight += 1;
        } else if let Some(r) = rec {
            self.write(r);
        }
        self.in_flight = self.in_flight.saturating_sub(1);
    }

    /// Boundary: no further admissions. Returns the frames still in flight.
    fn seal(&mut self) -> u64 {
        self.phase = Phase::Sealed;
        self.in_flight
    }

    /// End of the drain: nothing may be written any more. Returns the frames left in flight.
    fn close(&mut self) -> u64 {
        self.phase = Phase::Closed;
        self.in_flight
    }

    fn write(&mut self, rec: &Value) {
        if self.write_error || self.limit_reached {
            self.dropped += 1;
            return;
        }
        let line = rec.to_string();
        if self.bytes + line.len() as u64 + 1 > MAX_RUN_BYTES {
            self.limit_reached = true;
            self.dropped += 1;
            return;
        }
        if self
            .file
            .write_all(line.as_bytes())
            .and_then(|_| self.file.write_all(b"\n"))
            .is_err()
        {
            self.write_error = true;
            self.dropped += 1;
            return;
        }
        self.lines += 1;
        self.bytes += line.len() as u64 + 1;
    }
}

fn dir_usage(dir: &Path) -> std::io::Result<(u64, usize)> {
    let mut bytes = 0;
    let mut files = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file() {
            bytes += meta.len();
            files += 1;
        }
    }
    Ok((bytes, files))
}

fn write_manifest(path: &Path, manifest: &Value) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(manifest.to_string().as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Ok(dir) = File::open(path.parent().unwrap_or(Path::new("."))) {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn next_run_number(dir: &Path) -> u32 {
    let mut n = 1;
    while dir.join(format!("run-{n}.manifest.json")).exists() {
        n += 1;
    }
    n
}

fn mark_stale_runs_crashed(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.ends_with(".manifest.json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(mut m) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if m.get("reason") == Some(&json!("running")) {
            m["complete"] = json!(false);
            m["reason"] = json!("crashed");
            let _ = write_manifest(&path, &m);
        }
    }
}

fn terminate_child(child: &mut Child, grace: Duration) -> Option<ExitStatus> {
    let pid = child.id() as libc::pid_t;
    // SAFETY: plain kill(2) on our own child.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                return child.wait().ok();
            }
            Err(_) => return None,
        }
    }
}

fn status_json(status: Option<ExitStatus>) -> Value {
    use std::os::unix::process::ExitStatusExt;
    match status {
        None => json!(null),
        Some(s) => json!({ "code": s.code(), "signal": s.signal() }),
    }
}

fn exit_code_for(status: Option<ExitStatus>) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    match status {
        Some(s) => s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)),
        None => 70,
    }
}

fn inspect_launch(out: &Path) -> i32 {
    let _ = fs::create_dir_all(out);
    let argv: Vec<String> = std::env::args().collect();
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let env_count = std::env::vars_os().count();
    let env_names_known: Vec<&str> = [
        "HOME",
        "PATH",
        "TMPDIR",
        "CLAUDE_PROJECT_DIR",
        "USER",
        "SHELL",
        "LANG",
    ]
    .into_iter()
    .filter(|k| std::env::var_os(k).is_some())
    .collect();
    let rec = json!({
        "kind": "launch",
        "unix_ms": unix_ms(),
        "argv": argv,
        "cwd": cwd,
        "env_count": env_count,
        "env_known_present": env_names_known,
    });
    eprintln!("evidence-relay inspect-launch marker");
    let path = out.join(format!("launch-{}.json", unix_ms()));
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut f) => {
            let _ = f.write_all(rec.to_string().as_bytes());
            0
        }
        Err(_) => 1,
    }
}

fn main() {
    let args = parse_args();
    if args.inspect_launch {
        std::process::exit(inspect_launch(&args.out));
    }
    install_signal_handlers();
    let start = Instant::now();
    if fs::create_dir(&args.out).is_ok() {
        let _ = fs::set_permissions(
            &args.out,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        );
    }
    let lock_path = args.out.join(".controller.lock");
    let lock_file = match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("evidence-relay: cannot open lock file: {e}");
            std::process::exit(70);
        }
    };
    // SAFETY: flock on a file we own; LOCK_NB makes this non-blocking.
    let locked = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if !locked {
        eprintln!("evidence-relay: evidence directory busy");
        std::process::exit(75);
    }
    mark_stale_runs_crashed(&args.out);
    let (used_bytes, used_files) = dir_usage(&args.out).unwrap_or((u64::MAX, usize::MAX));
    if used_bytes.saturating_add(MAX_RUN_BYTES) > MAX_DIR_BYTES || used_files + 2 > MAX_DIR_FILES {
        eprintln!("evidence-relay: storage budget exhausted");
        std::process::exit(73);
    }
    let run = next_run_number(&args.out);
    let manifest_path = args.out.join(format!("run-{run}.manifest.json"));
    let records_path = args.out.join(format!("run-{run}.records.jsonl"));
    let records_file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&records_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("evidence-relay: cannot create records file: {e}");
            std::process::exit(70);
        }
    };
    let mut manifest = json!({
        "run": run,
        "complete": false,
        "reason": "running",
        "started_unix_ms": unix_ms(),
        "server": args.server,
        "expect_nonce_configured": args.expect_nonce.is_some(),
        "records_file": records_path.file_name().and_then(|n| n.to_str()),
    });
    if let Err(e) = write_manifest(&manifest_path, &manifest) {
        eprintln!("evidence-relay: cannot write manifest: {e}");
        std::process::exit(70);
    }

    let mut child = match Command::new(&args.server[0])
        .args(&args.server[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            manifest["reason"] = json!("spawn_failed");
            let _ = write_manifest(&manifest_path, &manifest);
            eprintln!("evidence-relay: spawn failed: {e}");
            std::process::exit(70);
        }
    };
    manifest["child_pid"] = json!(child.id());
    let _ = write_manifest(&manifest_path, &manifest);

    let recorder = Arc::new(Mutex::new(Recorder::new(records_file)));
    let expect = args.expect_nonce.clone();
    let pending: Arc<Mutex<Pending>> = Arc::new(Mutex::new(Pending::default()));
    let frames_in = Arc::new(AtomicU64::new(0));
    let frames_out = Arc::new(AtomicU64::new(0));
    let inbound_outcome = Arc::new(AtomicU8::new(Outcome::Running as u8));
    let outbound_outcome = Arc::new(AtomicU8::new(Outcome::Running as u8));
    let stdin_eof = Arc::new(AtomicBool::new(false));
    let child_stdout_closed = Arc::new(AtomicBool::new(false));

    let mut child_stdin = child.stdin.take().expect("piped stdin");
    let child_stdout = child.stdout.take().expect("piped stdout");

    // Client -> child.
    let inbound = {
        let recorder = recorder.clone();
        let expect = expect.clone();
        let pending = pending.clone();
        let frames_in = frames_in.clone();
        let stdin_eof = stdin_eof.clone();
        let outcome = inbound_outcome.clone();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            let mut reader = BufReader::new(stdin.lock());
            let mut buf = Vec::with_capacity(4096);
            let result = loop {
                buf.clear();
                match read_bounded_line(&mut reader, &mut buf) {
                    Ok(0) => break Outcome::CleanEof,
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                        break Outcome::FramingError;
                    }
                    Err(_) => break Outcome::ReadError,
                }
                // Admission under the recorder lock: past the boundary the frame is late and is
                // neither recorded nor forwarded.
                if !recorder.lock().unwrap_or_else(|e| e.into_inner()).admit() {
                    continue;
                }
                test_pause_point(&buf);
                let elapsed = start.elapsed().as_millis() as u64;
                let trimmed = strip_newline(&buf);
                let rec = if trimmed.is_empty() {
                    None
                } else {
                    Some(record(
                        "in",
                        trimmed,
                        elapsed,
                        expect.as_deref(),
                        &mut pending.lock().unwrap_or_else(|e| e.into_inner()),
                    ))
                };
                recorder
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .publish(rec.as_ref());
                if rec.is_some() {
                    frames_in.fetch_add(1, Ordering::Relaxed);
                }
                if child_stdin
                    .write_all(&buf)
                    .and_then(|_| child_stdin.flush())
                    .is_err()
                {
                    break Outcome::ForwardError;
                }
            };
            // Only a clean EOF is an orderly end of input. Any failure still closes the child's
            // stdin so the child can exit, and leaves the run incomplete.
            outcome.store(result as u8, Ordering::SeqCst);
            if result == Outcome::CleanEof {
                stdin_eof.store(true, Ordering::SeqCst);
            }
            drop(child_stdin);
        })
    };

    // Child -> client.
    let outbound = {
        let recorder = recorder.clone();
        let expect = expect.clone();
        let pending = pending.clone();
        let frames_out = frames_out.clone();
        let closed = child_stdout_closed.clone();
        let outcome = outbound_outcome.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(child_stdout);
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let mut buf = Vec::with_capacity(4096);
            let result = loop {
                buf.clear();
                match read_bounded_line(&mut reader, &mut buf) {
                    Ok(0) => break Outcome::CleanEof,
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                        break Outcome::FramingError;
                    }
                    Err(_) => break Outcome::ReadError,
                }
                if !recorder.lock().unwrap_or_else(|e| e.into_inner()).admit() {
                    continue;
                }
                let elapsed = start.elapsed().as_millis() as u64;
                let trimmed = strip_newline(&buf);
                let rec = if trimmed.is_empty() {
                    None
                } else {
                    Some(record(
                        "out",
                        trimmed,
                        elapsed,
                        expect.as_deref(),
                        &mut pending.lock().unwrap_or_else(|e| e.into_inner()),
                    ))
                };
                recorder
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .publish(rec.as_ref());
                if rec.is_some() {
                    frames_out.fetch_add(1, Ordering::Relaxed);
                }
                // The client may stop reading. A blocked write here never blocks finalization,
                // which waits for this thread only for a bounded time.
                if out.write_all(&buf).and_then(|_| out.flush()).is_err() {
                    break Outcome::ForwardError;
                }
            };
            outcome.store(result as u8, Ordering::SeqCst);
            closed.store(true, Ordering::SeqCst);
        })
    };

    // Supervise: signals, child exit, stdout closure, stdin EOF.
    let mut status: Option<ExitStatus> = None;
    let reason: &str;
    let mut orderly = false;
    let mut eof_at: Option<Instant> = None;
    loop {
        if GOT_SIGNAL.swap(false, Ordering::SeqCst) {
            let sig = SIGNAL_NO.load(Ordering::SeqCst) as libc::c_int;
            // SAFETY: forward the signal to our own child.
            unsafe {
                libc::kill(child.id() as libc::pid_t, sig);
            }
            orderly = true;
            reason = "signal";
            status = terminate_child(&mut child, CHILD_GRACE).or(status);
            break;
        }
        if let Ok(Some(s)) = child.try_wait() {
            status = Some(s);
            if stdin_eof.load(Ordering::SeqCst) {
                orderly = true;
                reason = "eof";
            } else {
                reason = "child_exited";
            }
            break;
        }
        if child_stdout_closed.load(Ordering::SeqCst) {
            // Child stdout closed while the child lives: bounded termination and reaping.
            if let Ok(Some(s)) = child.try_wait() {
                status = Some(s);
                orderly = stdin_eof.load(Ordering::SeqCst);
                reason = if orderly { "eof" } else { "child_exited" };
            } else {
                std::thread::sleep(Duration::from_millis(50));
                if let Ok(Some(s)) = child.try_wait() {
                    status = Some(s);
                    orderly = stdin_eof.load(Ordering::SeqCst);
                    reason = if orderly { "eof" } else { "child_exited" };
                } else {
                    reason = "child_stdout_closed";
                    status = terminate_child(&mut child, CHILD_GRACE);
                }
            }
            break;
        }
        let inbound_now = Outcome::from_u8(inbound_outcome.load(Ordering::SeqCst));
        if stdin_eof.load(Ordering::SeqCst)
            || matches!(
                inbound_now,
                Outcome::FramingError | Outcome::ReadError | Outcome::ForwardError
            )
        {
            let at = *eof_at.get_or_insert_with(Instant::now);
            if at.elapsed() > EOF_WAIT {
                reason = "eof_timeout";
                status = terminate_child(&mut child, CHILD_GRACE);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Capture boundary, synchronized through the recorder lock: seal (no new admissions), drain
    // the frames admitted before the seal for a bounded time, then close (nothing may be written).
    // A frame that passed admission is either published before the recount or dropped and
    // counted after the close; it never appends to the finalized record set.
    let in_flight_at_seal = recorder.lock().unwrap_or_else(|e| e.into_inner()).seal();
    let drain_deadline = Instant::now() + Duration::from_millis(IN_FLIGHT_DRAIN_MS);
    while recorder.lock().unwrap_or_else(|e| e.into_inner()).in_flight > 0
        && Instant::now() < drain_deadline
    {
        std::thread::sleep(Duration::from_millis(2));
    }
    let in_flight_unresolved = recorder.lock().unwrap_or_else(|e| e.into_inner()).close();
    // Bounded drain of the forwarding threads. The outbound thread may be blocked writing to a
    // client that stopped reading; the inbound thread may be blocked on an open stdin. Neither may
    // delay finalization beyond THREAD_DRAIN; a thread still running makes the run incomplete and
    // dies with the process.
    let outbound_done = wait_flag(&outbound_outcome, THREAD_DRAIN);
    if outbound_done {
        let _ = outbound.join();
    }
    let mut inbound_now = Outcome::from_u8(inbound_outcome.load(Ordering::SeqCst));
    if inbound_now != Outcome::Running {
        let _ = inbound.join();
    } else {
        // Input still open. The boundary is quiescent only when stdin has no data pending within a
        // short window. A hang-up means the client is closing its end: the inbound thread is given
        // a moment to observe EOF. Pending data means frames exist that were not captured.
        match stdin_state(QUIESCENCE_MS) {
            StdinState::Idle => {
                inbound_now = Outcome::Quiescent;
                inbound_outcome.store(Outcome::Quiescent as u8, Ordering::SeqCst);
            }
            StdinState::HangUp | StdinState::Data => {
                // Readable: either EOF (the thread reports clean_eof) or a frame past the boundary
                // (counted as late; the thread stays running and the run is incomplete).
                if wait_flag(&inbound_outcome, Duration::from_millis(EOF_GRACE_MS)) {
                    inbound_now = Outcome::from_u8(inbound_outcome.load(Ordering::SeqCst));
                    let _ = inbound.join();
                }
            }
            StdinState::Error => {}
        }
    }
    let outbound_now = Outcome::from_u8(outbound_outcome.load(Ordering::SeqCst));

    // Finalize.
    let (lines, bytes, dropped, write_error, limit_reached) = {
        let mut r = recorder.lock().unwrap_or_else(|e| e.into_inner());
        let flushed = r.file.flush().is_ok() && r.file.get_ref().sync_all().is_ok();
        if !flushed {
            r.write_error = true;
        }
        (r.lines, r.bytes, r.dropped, r.write_error, r.limit_reached)
    };
    let recount = File::open(&records_path)
        .map(|f| BufReader::new(f).lines().count() as u64)
        .unwrap_or(u64::MAX);
    let threads_clean = matches!(inbound_now, Outcome::CleanEof | Outcome::Quiescent);
    let output_clean = outbound_now == Outcome::CleanEof;
    let complete = orderly
        && status.is_some()
        && threads_clean
        && output_clean
        && in_flight_unresolved == 0
        && !write_error
        && !limit_reached
        && dropped == 0
        && recount == lines;
    let reason = if complete {
        "orderly"
    } else if matches!(inbound_now, Outcome::FramingError) {
        "input_framing_error"
    } else if matches!(inbound_now, Outcome::ReadError) {
        "input_read_error"
    } else if matches!(inbound_now, Outcome::ForwardError) {
        "forward_to_child_failed"
    } else if matches!(outbound_now, Outcome::ForwardError) {
        "client_write_failed"
    } else if matches!(outbound_now, Outcome::FramingError) {
        "child_output_framing_error"
    } else if matches!(outbound_now, Outcome::ReadError) {
        "child_output_read_error"
    } else if outbound_now == Outcome::Running {
        "client_output_blocked"
    } else if in_flight_unresolved > 0 {
        "frame_in_flight"
    } else if inbound_now == Outcome::Running {
        "input_pending"
    } else {
        reason
    };
    manifest["complete"] = json!(complete);
    manifest["reason"] = json!(reason);
    manifest["inbound_outcome"] = json!(inbound_now.as_str());
    manifest["outbound_outcome"] = json!(outbound_now.as_str());
    manifest["pending_correlations"] =
        json!(pending.lock().unwrap_or_else(|e| e.into_inner()).len());
    {
        let r = recorder.lock().unwrap_or_else(|e| e.into_inner());
        manifest["late_frames"] = json!(r.late);
        manifest["in_flight_at_seal"] = json!(in_flight_at_seal);
        manifest["in_flight_unresolved"] = json!(in_flight_unresolved);
        manifest["dropped_in_flight"] = json!(r.dropped_in_flight);
    }
    if !complete {
        eprintln!("evidence-relay: run {run} incomplete: {reason}");
    }
    manifest["ended_unix_ms"] = json!(unix_ms());
    manifest["elapsed_ms"] = json!(start.elapsed().as_millis() as u64);
    manifest["child_exit"] = status_json(status);
    manifest["records"] = json!(lines);
    manifest["records_recount"] = json!(recount);
    manifest["record_bytes"] = json!(bytes);
    manifest["dropped"] = json!(dropped);
    manifest["write_error"] = json!(write_error);
    manifest["limit_reached"] = json!(limit_reached);
    manifest["frames_in"] = json!(frames_in.load(Ordering::Relaxed));
    manifest["frames_out"] = json!(frames_out.load(Ordering::Relaxed));
    if let Ok(data) = fs::read(&records_path) {
        manifest["records_sha256"] = json!(
            Sha256::digest(&data)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
    }
    let _ = write_manifest(&manifest_path, &manifest);
    std::process::exit(exit_code_for(status));
}

fn strip_newline(buf: &[u8]) -> &[u8] {
    let mut line = buf;
    if let Some(s) = line.strip_suffix(b"\n") {
        line = s;
    }
    if let Some(s) = line.strip_suffix(b"\r") {
        line = s;
    }
    line
}

/// Reads one line of at most MAX_FRAME bytes. Longer lines are an error.
fn read_bounded_line<R: Read>(
    reader: &mut BufReader<R>,
    buf: &mut Vec<u8>,
) -> std::io::Result<usize> {
    loop {
        let (take, newline) = {
            let avail = reader.fill_buf()?;
            if avail.is_empty() {
                return Ok(buf.len());
            }
            match avail.iter().position(|&b| b == b'\n') {
                Some(pos) => (pos + 1, true),
                None => (avail.len(), false),
            }
        };
        if buf.len() + take > MAX_FRAME {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame exceeds relay limit",
            ));
        }
        let avail = reader.fill_buf()?;
        buf.extend_from_slice(&avail[..take]);
        reader.consume(take);
        if newline {
            return Ok(buf.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorder() -> (Recorder, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "evidence-relay-unit-{}-{}.jsonl",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        (Recorder::new(file), path)
    }

    fn lines_on_disk(r: &mut Recorder, path: &Path) -> usize {
        r.file.flush().unwrap();
        BufReader::new(File::open(path).unwrap()).lines().count()
    }

    #[test]
    fn frames_admitted_before_the_seal_publish_before_the_close_and_never_after() {
        let (mut r, path) = recorder();
        assert!(r.admit());
        r.publish(Some(&json!({"a": 1})));
        assert_eq!((r.lines, r.in_flight), (1, 0));
        // Admitted before the seal, published during the drain: included.
        assert!(r.admit());
        assert_eq!(r.seal(), 1);
        r.publish(Some(&json!({"b": 2})));
        assert_eq!((r.lines, r.in_flight), (2, 0));
        // Admitted before the seal, published after the close: dropped and counted, not written.
        let _ = r.close();
        let (mut r2, path2) = recorder();
        assert!(r2.admit());
        assert_eq!(r2.seal(), 1);
        assert_eq!(r2.close(), 1);
        r2.publish(Some(&json!({"c": 3})));
        assert_eq!((r2.lines, r2.in_flight, r2.dropped_in_flight), (0, 0, 1));
        assert_eq!(lines_on_disk(&mut r2, &path2), 0);
        // Admission after the seal is late and never published.
        assert!(!r.admit());
        assert!(!r2.admit());
        assert_eq!((r.late, r2.late), (1, 1));
        assert_eq!(lines_on_disk(&mut r, &path), 2);
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(path2);
    }
}
