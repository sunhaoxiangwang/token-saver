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

/// Terminal state of a forwarding thread. Anything other than a clean end makes the run incomplete.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Outcome {
    Running = 0,
    CleanEof = 1,
    FramingError = 2,
    ReadError = 3,
    ForwardError = 4,
}

impl Outcome {
    fn from_u8(v: u8) -> Outcome {
        match v {
            1 => Outcome::CleanEof,
            2 => Outcome::FramingError,
            3 => Outcome::ReadError,
            4 => Outcome::ForwardError,
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

fn is_nonce_shape(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric())
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
                        json!(if tool == "connection_probe" {
                            tool
                        } else {
                            "other"
                        }),
                    );
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
                if let Some(k) = &id_key {
                    pending.remove(k);
                }
                if let Some(code) = err.get("code").and_then(Value::as_i64) {
                    obj.insert("error_code".into(), json!(code));
                }
            } else if let Some(result) = map.get("result") {
                obj.insert("kind".into(), json!("response"));
                let for_call =
                    id_key.as_ref().and_then(|k| pending.remove(k)) == Some("tools/call");
                if for_call && let Some(r) = result.as_object() {
                    if r.get("isError") == Some(&Value::Bool(true)) {
                        obj.insert("tool_error".into(), json!(true));
                    }
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

struct Recorder {
    file: BufWriter<File>,
    lines: u64,
    bytes: u64,
    dropped: u64,
    write_error: bool,
    limit_reached: bool,
}

impl Recorder {
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

    let recorder = Arc::new(Mutex::new(Recorder {
        file: BufWriter::new(records_file),
        lines: 0,
        bytes: 0,
        dropped: 0,
        write_error: false,
        limit_reached: false,
    }));
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
                let elapsed = start.elapsed().as_millis() as u64;
                let trimmed = strip_newline(&buf);
                if !trimmed.is_empty() {
                    let rec = record(
                        "in",
                        trimmed,
                        elapsed,
                        expect.as_deref(),
                        &mut pending.lock().unwrap_or_else(|e| e.into_inner()),
                    );
                    recorder
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .write(&rec);
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
                let elapsed = start.elapsed().as_millis() as u64;
                let trimmed = strip_newline(&buf);
                if !trimmed.is_empty() {
                    let rec = record(
                        "out",
                        trimmed,
                        elapsed,
                        expect.as_deref(),
                        &mut pending.lock().unwrap_or_else(|e| e.into_inner()),
                    );
                    recorder
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .write(&rec);
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
    // Bounded drain of the forwarding threads. The outbound thread may be blocked writing to a
    // client that stopped reading; the inbound thread may be blocked on an open stdin. Neither may
    // delay finalization beyond THREAD_DRAIN; a thread still running makes the run incomplete and
    // dies with the process.
    let outbound_done = wait_flag(&outbound_outcome, THREAD_DRAIN);
    if outbound_done {
        let _ = outbound.join();
    }
    let inbound_now = Outcome::from_u8(inbound_outcome.load(Ordering::SeqCst));
    if inbound_now != Outcome::Running {
        let _ = inbound.join();
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
    let threads_clean =
        inbound_now == Outcome::CleanEof || (reason == "signal" && inbound_now == Outcome::Running);
    let output_clean = outbound_now == Outcome::CleanEof;
    let complete = orderly
        && status.is_some()
        && threads_clean
        && output_clean
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
    } else {
        reason
    };
    manifest["complete"] = json!(complete);
    manifest["reason"] = json!(reason);
    manifest["inbound_outcome"] = json!(inbound_now.as_str());
    manifest["outbound_outcome"] = json!(outbound_now.as_str());
    manifest["pending_correlations"] =
        json!(pending.lock().unwrap_or_else(|e| e.into_inner()).len());
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
