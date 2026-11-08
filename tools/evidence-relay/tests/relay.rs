//! Evidence relay tests: allowlisted persistence, canary exclusion, manifest completeness,
//! directory locking, budget exhaustion, child cleanup, and signal forwarding.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const RELAY: &str = env!("CARGO_BIN_EXE_evidence-relay");

fn tmpdir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("evidence-relay-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn spawn_relay(out: &Path, nonce: Option<&str>, server: &[&str]) -> Child {
    let mut cmd = Command::new(RELAY);
    if let Some(n) = nonce {
        cmd.args(["--expect-nonce", n]);
    }
    cmd.arg("--out").arg(out).arg("--").args(server);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    cmd.spawn().expect("spawn relay")
}

fn wait_exit(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => return None,
        }
    }
}

/// Waits until the relay has written its running manifest with a child pid, which happens only
/// after its lock is held and before any forwarding starts.
fn wait_ready(out: &Path, run: u32, timeout: Duration) {
    let path = out.join(format!("run-{run}.manifest.json"));
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(text) = fs::read_to_string(&path)
            && let Ok(v) = serde_json::from_str::<Value>(&text)
            && v.get("child_pid").is_some()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("relay did not become ready");
}

fn manifest(out: &Path, run: u32) -> Value {
    let text = fs::read_to_string(out.join(format!("run-{run}.manifest.json"))).expect("manifest");
    serde_json::from_str(&text).unwrap()
}

fn records(out: &Path, run: u32) -> Vec<Value> {
    let f = fs::File::open(out.join(format!("run-{run}.records.jsonl"))).expect("records");
    BufReader::new(f)
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect()
}

#[test]
fn echo_server_round_trip_persists_allowlisted_fields_and_completes() {
    let out = tmpdir("echo");
    let canary = "CANARY_RELAY_91";
    let mut relay = spawn_relay(&out, Some("abc123"), &["/bin/cat"]);
    let mut stdin = relay.stdin.take().unwrap();
    let frames = [
        json!({"jsonrpc":"2.0","id":format!("{canary}id"),"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":format!("{canary}client"),"version":format!("{canary}v")},"capabilities":{}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"connection_probe","arguments":{"nonce":"abc123"}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"connection_probe","arguments":{"nonce":format!("{canary}nonce")}}}),
        json!({"jsonrpc":"2.0","id":4,"method":format!("{canary}/method"),"params":{"_meta":{"io.modelcontextprotocol/protocolVersion":format!("{canary}ver")}}}),
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2,"reason":format!("{canary}reason")}}),
    ];
    for f in &frames {
        writeln!(stdin, "{f}").unwrap();
    }
    writeln!(stdin, "{{not json {canary}").unwrap();
    stdin.flush().unwrap();
    drop(stdin);
    let status = wait_exit(&mut relay, Duration::from_secs(10)).expect("relay exits after EOF");
    assert_eq!(status.code(), Some(0));
    let m = manifest(&out, 1);
    assert_eq!(m["complete"], true, "{m}");
    assert_eq!(m["reason"], "orderly");
    assert_eq!(m["dropped"], 0);
    let recs = records(&out, 1);
    assert_eq!(recs.len() as u64, m["records"].as_u64().unwrap());
    assert_eq!(m["records_recount"], m["records"]);
    let text = fs::read_to_string(out.join("run-1.records.jsonl")).unwrap();
    assert!(!text.contains(canary), "canary persisted: {text}");
    assert!(
        !text.contains("abc123") || recs.iter().any(|r| r["nonce_match"] == true),
        "nonce only as match flag"
    );
    assert!(
        recs.iter()
            .any(|r| r["dir"] == "in" && r["method"] == "tools/call" && r["nonce_match"] == true)
    );
    assert!(recs.iter().any(|r| r["dir"] == "in"
        && r["method"] == "tools/call"
        && r["nonce_match"] == false
        && r["nonce_sha256"].is_string()));
    assert!(recs.iter().any(|r| r["method"] == "other"));
    assert!(recs.iter().any(|r| r["kind"] == "unparsable"));
    assert!(
        recs.iter().any(|r| r["dir"] == "out"),
        "echoed frames recorded as outbound"
    );
    assert!(
        recs.iter()
            .all(|r| r.get("client_name").is_none() && r.get("client_version").is_none())
    );
    assert!(recs.iter().any(|r| r["protocol_version"] == "2025-11-25"));
    assert!(
        recs.iter()
            .filter(|r| r.get("meta_protocol_version").is_some())
            .all(|r| r["meta_protocol_version"] == "other")
    );
}

#[test]
fn concurrent_launch_is_refused_while_directory_is_busy() {
    let out = tmpdir("busy");
    let mut first = spawn_relay(&out, None, &["/bin/cat"]);
    wait_ready(&out, 1, Duration::from_secs(5));
    let mut second = spawn_relay(&out, None, &["/bin/cat"]);
    let status = wait_exit(&mut second, Duration::from_secs(5)).expect("second exits");
    assert_eq!(status.code(), Some(75));
    drop(first.stdin.take());
    wait_exit(&mut first, Duration::from_secs(5));
}

#[test]
fn storage_budget_exhaustion_refuses_to_start() {
    let out = tmpdir("budget");
    fs::create_dir_all(&out).unwrap();
    let big = fs::File::create(out.join("raw-artifact.bin")).unwrap();
    big.set_len(60 * 1024 * 1024).unwrap();
    let mut relay = spawn_relay(&out, None, &["/bin/cat"]);
    let status = wait_exit(&mut relay, Duration::from_secs(5)).expect("exits");
    assert_eq!(status.code(), Some(73));
}

#[test]
fn stale_running_manifest_is_marked_crashed_and_child_stdout_closure_is_incomplete() {
    let out = tmpdir("stale");
    fs::create_dir_all(&out).unwrap();
    fs::write(
        out.join("run-1.manifest.json"),
        json!({"run":1,"complete":false,"reason":"running"}).to_string(),
    )
    .unwrap();
    // A child that closes stdout but keeps running.
    let mut relay = spawn_relay(&out, None, &["/bin/sh", "-c", "exec 1>&-; sleep 30"]);
    let t0 = Instant::now();
    let status = wait_exit(&mut relay, Duration::from_secs(8)).expect("relay reaps the child");
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "bounded termination: {:?}",
        t0.elapsed()
    );
    assert_ne!(status.code(), Some(0));
    let stale = manifest(&out, 1);
    assert_eq!(stale["reason"], "crashed");
    assert_eq!(stale["complete"], false);
    let m = manifest(&out, 2);
    assert_eq!(m["complete"], false);
    assert_eq!(m["reason"], "child_stdout_closed");
    assert!(m["child_exit"].is_object());
}

#[test]
fn signal_is_forwarded_and_child_is_reaped_with_status_preserved() {
    let out = tmpdir("signal");
    let mut relay = spawn_relay(&out, None, &["/bin/sleep", "30"]);
    wait_ready(&out, 1, Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(50));
    unsafe {
        libc::kill(relay.id() as libc::pid_t, libc::SIGTERM);
    }
    let status = wait_exit(&mut relay, Duration::from_secs(5)).expect("relay exits after signal");
    assert_eq!(
        status.code(),
        Some(128 + 15),
        "child killed by SIGTERM, status preserved"
    );
    let m = manifest(&out, 1);
    assert_eq!(
        m["complete"], true,
        "orderly signal shutdown with no records lost: {m}"
    );
    assert_eq!(m["child_exit"]["signal"], 15);
    // The input was open but idle at the capture boundary.
    assert_eq!(m["inbound_outcome"], "quiescent");
    assert_eq!(m["late_frames"], 0);
}

#[test]
fn input_after_the_capture_boundary_is_late_or_pending_never_silently_complete() {
    let out = tmpdir("late-input");
    let mut relay = spawn_relay(&out, None, &["/bin/sleep", "30"]);
    wait_ready(&out, 1, Duration::from_secs(5));
    let mut stdin = relay.stdin.take().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    unsafe {
        libc::kill(relay.id() as libc::pid_t, libc::SIGTERM);
    }
    // Written while the relay finalizes: either read after the boundary (late, not recorded) or
    // still pending on stdin (the capture is incomplete). It is never recorded as part of a
    // complete capture.
    for _ in 0..20 {
        let _ = writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":"LATE_CANARY_55","method":"tools/call","params":{"name":"text_search","arguments":{"query":"LATE_CANARY_55"}}})
        );
        let _ = stdin.flush();
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = wait_exit(&mut relay, Duration::from_secs(5)).expect("relay exits after signal");
    let m = manifest(&out, 1);
    let recs = records(&out, 1);
    let late = m["late_frames"].as_u64().unwrap_or(0);
    let recorded_late = recs.iter().filter(|r| r["method"] == "tools/call").count() as u64;
    if m["complete"] == true {
        assert!(
            m["inbound_outcome"] == "quiescent" || m["inbound_outcome"] == "clean_eof",
            "{m}"
        );
        assert_eq!(
            recorded_late, 0,
            "frames after the boundary must not be recorded: {m}"
        );
    } else {
        assert!(
            m["reason"] == "input_pending" || m["reason"] == "forward_to_child_failed",
            "{m}"
        );
    }
    assert!(
        late > 0 || recorded_late > 0 || m["complete"] == false,
        "{m}"
    );
    let text = fs::read_to_string(out.join("run-1.records.jsonl")).unwrap();
    assert!(!text.contains("LATE_CANARY_55"));
}

#[test]
fn inspect_launch_records_argv_cwd_and_env_count_without_values() {
    let out = tmpdir("launch");
    let status = Command::new(RELAY)
        .args(["--inspect-launch", "--out"])
        .arg(&out)
        .env("TS_SECRET_CANARY", "topsecret-value")
        .current_dir(std::env::temp_dir())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let entry = fs::read_dir(&out)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("launch-"))
        .unwrap();
    let text = fs::read_to_string(entry.path()).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["kind"], "launch");
    assert!(v["argv"].as_array().unwrap().len() >= 3);
    assert!(v["env_count"].as_u64().unwrap() >= 1);
    assert!(
        !text.contains("topsecret-value"),
        "environment values are never persisted"
    );
}

#[test]
fn oversized_input_frame_marks_the_run_incomplete() {
    let out = tmpdir("oversized");
    let mut relay = spawn_relay(&out, None, &["/bin/cat"]);
    let mut stdin = relay.stdin.take().unwrap();
    let big = vec![b'a'; 65_537];
    let _ = stdin.write_all(&big);
    let _ = stdin.flush();
    drop(stdin);
    let status = wait_exit(&mut relay, Duration::from_secs(10)).expect("relay exits");
    let _ = status;
    let m = manifest(&out, 1);
    assert_eq!(m["complete"], false, "{m}");
    assert_eq!(m["reason"], "input_framing_error");
    assert_eq!(m["inbound_outcome"], "framing_error");
}

/// A child that floods its stdout with 3,000 short frames on start, then behaves like `cat`. The
/// relay's stdout is a pipe the test never reads, so the relay's outbound write blocks after the
/// pipe fills. No large write by the test itself is needed, so the test cannot block.
const FLOODING_CAT: &str = r#"i=0; while [ $i -lt 3000 ]; do echo '{"jsonrpc":"2.0","method":"notifications/progress","params":{"pad":"pppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppppp"}}'; i=$((i+1)); done; exec cat"#;

fn spawn_blocked_output_relay(out: &Path) -> Child {
    let relay = spawn_relay(out, None, &["/bin/sh", "-c", FLOODING_CAT]);
    wait_ready(out, 1, Duration::from_secs(5));
    // Let the flood fill the unread stdout pipe and block the outbound thread.
    std::thread::sleep(Duration::from_millis(800));
    relay
}

#[test]
fn blocked_client_stdout_then_signal_terminates_within_bound_and_is_incomplete() {
    let out = tmpdir("blocked-signal");
    let mut relay = spawn_blocked_output_relay(&out);
    let _stdin = relay.stdin.take();
    let t0 = Instant::now();
    unsafe {
        libc::kill(relay.id() as libc::pid_t, libc::SIGTERM);
    }
    let status = wait_exit(&mut relay, Duration::from_secs(6))
        .expect("relay terminates despite blocked stdout");
    assert!(t0.elapsed() < Duration::from_secs(4), "{:?}", t0.elapsed());
    assert_eq!(status.code(), Some(128 + 15));
    let m = manifest(&out, 1);
    assert_eq!(m["complete"], false, "{m}");
    assert_eq!(m["reason"], "client_output_blocked");
    assert_eq!(m["outbound_outcome"], "running");
}

#[test]
fn blocked_client_stdout_then_stdin_eof_terminates_within_bound_and_is_incomplete() {
    let out = tmpdir("blocked-eof");
    let mut relay = spawn_blocked_output_relay(&out);
    let t0 = Instant::now();
    drop(relay.stdin.take());
    let status = wait_exit(&mut relay, Duration::from_secs(8))
        .expect("relay terminates after EOF with blocked stdout");
    assert!(t0.elapsed() < Duration::from_secs(6), "{:?}", t0.elapsed());
    // The child cannot finish flushing into a blocked pipe, so the relay ends it after its EOF
    // wait; either status is acceptable, the run must be incomplete and bounded.
    assert!(matches!(status.code(), Some(0) | Some(143)), "{status:?}");
    let m = manifest(&out, 1);
    assert_eq!(m["complete"], false, "{m}");
    assert_eq!(m["reason"], "client_output_blocked");
    assert!(m["child_exit"].is_object(), "child reaped");
}

#[test]
fn correlation_state_is_bounded_and_retires_on_errors() {
    // Echo server: every request comes back as a request, never answered, so entries only retire
    // by eviction at the bound.
    let out = tmpdir("pending-bound");
    let mut relay = spawn_relay(&out, Some("n"), &["/bin/cat"]);
    {
        let mut stdin = relay.stdin.take().unwrap();
        let mut reader = BufReader::new(relay.stdout.take().unwrap());
        for i in 0..5_000u64 {
            writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":i,"method":"tools/call","params":{"name":"connection_probe","arguments":{"nonce":"n"}}})).unwrap();
            let mut echoed = String::new();
            reader.read_line(&mut echoed).unwrap();
        }
        drop(stdin);
    }
    wait_exit(&mut relay, Duration::from_secs(10)).expect("relay exits");
    let m = manifest(&out, 1);
    assert!(m["pending_correlations"].as_u64().unwrap() <= 1024, "{m}");

    // Error-answering server: each request is answered with an RPC error, retiring its entry.
    let out = tmpdir("pending-errors");
    let script = r#"while IFS= read -r line; do printf '%s
' '{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"x"}}'; done"#;
    let mut relay = spawn_relay(&out, Some("n"), &["/bin/sh", "-c", script]);
    {
        let mut stdin = relay.stdin.take().unwrap();
        let mut reader = BufReader::new(relay.stdout.take().unwrap());
        for _ in 0..500u64 {
            writeln!(
                stdin,
                "{}",
                json!({"jsonrpc":"2.0","id":7,"method":"nope/x","params":{}})
            )
            .unwrap();
            let mut answer = String::new();
            reader.read_line(&mut answer).unwrap();
        }
        drop(stdin);
    }
    wait_exit(&mut relay, Duration::from_secs(10)).expect("relay exits");
    let m = manifest(&out, 1);
    assert_eq!(m["pending_correlations"], 0, "{m}");
    assert_eq!(m["complete"], true, "{m}");
}

// ----- Milestone 1: digests, canonical vectors, canary exclusion for workspace tools ---------

/// Sends frames through the echo relay and returns its records.
fn echo_records(name: &str, frames: &[Value]) -> (Vec<Value>, String) {
    let out = tmpdir(name);
    let mut relay = spawn_relay(&out, Some("abc123"), &["/bin/cat"]);
    let mut stdin = relay.stdin.take().unwrap();
    for f in frames {
        writeln!(stdin, "{f}").unwrap();
    }
    stdin.flush().unwrap();
    drop(stdin);
    let status = wait_exit(&mut relay, Duration::from_secs(10)).expect("relay exits after EOF");
    assert_eq!(status.code(), Some(0));
    assert_eq!(manifest(&out, 1)["complete"], true);
    let text = fs::read_to_string(out.join("run-1.records.jsonl")).unwrap();
    (records(&out, 1), text)
}

#[test]
fn canonical_argument_digests_match_the_plan_vectors() {
    // (arguments, canonical byte length, args_sha256) from TS-M1-PLAN-001 revision 4, section 7.3.
    let vectors: Vec<(Value, usize, &str)> = vec![
        (json!({}), 2, "44136fa355b3678a"),
        (json!({"path": ""}), 11, "518341a7f82a2945"),
        (
            json!({"max_lines": 60, "path": "src/main.rs", "start_line": 1}),
            52,
            "addece581e7e5c6f",
        ),
        (
            json!({"path": "src/main.rs", "start_line": 1, "max_lines": 60}),
            52,
            "addece581e7e5c6f",
        ),
        (
            json!({"case_sensitive": false, "context_lines": 0, "query": "outbound_too_large"}),
            71,
            "22d8cd22e833ca09",
        ),
        (
            json!({"max_bytes": 256, "path": "docs/été/日本.md"}),
            47,
            "78bbd623f9a8a6d3",
        ),
        (
            json!({"max_matches": 100, "query": "tab\there \"quoted\" back\\slash \u{1}ctl"}),
            72,
            "22d9e5e171640ea5",
        ),
        (
            json!({"depth": 3, "include_hidden": true, "max_entries": 200, "path": "a"}),
            62,
            "096afd0e9fb660cb",
        ),
        (
            json!({"b": {"z": [1, 2, {"y": null}], "a": "x"}, "A": "upper sorts first", "é": "non-ascii key"}),
            81,
            "77c4c3da126e8539",
        ),
    ];
    let frames: Vec<Value> = vectors
        .iter()
        .enumerate()
        .map(|(i, (args, _, _))| {
            json!({"jsonrpc":"2.0","id":i,"method":"tools/call","params":{"name":"workspace_outline","arguments":args}})
        })
        .collect();
    let (recs, _) = echo_records("vectors", &frames);
    let inbound: Vec<&Value> = recs
        .iter()
        .filter(|r| r["dir"] == "in" && r["method"] == "tools/call")
        .collect();
    assert_eq!(inbound.len(), vectors.len());
    for (rec, (args, bytes, digest)) in inbound.iter().zip(&vectors) {
        assert_eq!(rec["args_sha256"], *digest, "{args}");
        assert_eq!(rec["args_bytes"], *bytes, "{args}");
        assert_eq!(rec["tool"], "workspace_outline");
    }
    // A non-integer number cannot be canonicalized: digest null, flagged.
    let (recs, _) = echo_records(
        "noncanonical",
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"file_slice","arguments":{"path":"a","start_line":1.5}}}),
        ],
    );
    let r = recs.iter().find(|r| r["dir"] == "in").unwrap();
    assert!(r["args_sha256"].is_null());
    assert_eq!(r["args_canonical"], false);
}

#[test]
fn workspace_tool_responses_record_result_digests_and_never_content() {
    let canary_dir = "CANARY_DIR_7f3a";
    let canary_content = "CANARY_CONTENT_9c1e";
    let text = format!("d {canary_dir}\nf README.md 1024\n{canary_content}");
    let expected_digest = {
        use sha2::{Digest, Sha256};
        let d = Sha256::digest(text.as_bytes());
        d.iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let frames = [
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"workspace_outline","arguments":{"path":canary_dir}}}),
        json!({"jsonrpc":"2.0","id":7,"result":{"content":[{"type":"text","text":text}],"structuredContent":{"entries_listed":2,"truncated":false},"isError":false}}),
        json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"file_slice","arguments":{"path":canary_dir}}}),
        json!({"jsonrpc":"2.0","id":8,"result":{"content":[{"type":"text","text":"not found"}],"isError":true}}),
        json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"text_search","arguments":{"query":canary_content}}}),
        json!({"jsonrpc":"2.0","id":9,"error":{"code":-32603,"message":"processing deadline exceeded"}}),
        json!({"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"unknown_tool","arguments":{}}}),
    ];
    let (recs, raw) = echo_records("digests", &frames);
    assert!(
        !raw.contains(canary_dir) && !raw.contains(canary_content),
        "canary persisted: {raw}"
    );
    assert!(!raw.contains("README"), "entry name persisted");
    let ok = recs
        .iter()
        .find(|r| r["kind"] == "response" && r["dir"] == "in" && r["result_bytes"] == text.len())
        .expect("outline response record");
    assert_eq!(ok["result_sha256"], expected_digest);
    assert_eq!(ok["is_error"], false);
    assert!(ok.get("instance_id").is_none() && ok.get("status").is_none());
    let tool_err = recs
        .iter()
        .find(|r| r["kind"] == "response" && r["dir"] == "in" && r["is_error"] == true)
        .expect("tool error record");
    assert_eq!(tool_err["result_bytes"], "not found".len());
    assert_eq!(tool_err["tool_error"], true);
    let proto_err = recs
        .iter()
        .find(|r| r["kind"] == "error" && r["dir"] == "in")
        .expect("protocol error record");
    assert_eq!(proto_err["error_code"], -32603);
    assert!(proto_err["result_sha256"].is_null());
    let unknown = recs
        .iter()
        .find(|r| r["dir"] == "in" && r["id_sha256"].is_string() && r["tool"] == "other")
        .expect("unknown tool recorded as other");
    assert!(unknown["args_sha256"].is_string());
    // Every known tool name is recorded verbatim.
    let names: Vec<&str> = recs
        .iter()
        .filter(|r| r["dir"] == "in" && r["method"] == "tools/call")
        .filter_map(|r| r["tool"].as_str())
        .collect();
    assert_eq!(
        names,
        ["workspace_outline", "file_slice", "text_search", "other"]
    );
}

/// Spawns the relay with the test pause hook, feeds one `PAUSE` frame, and waits until the inbound
/// thread sits between admission and publication.
fn spawn_paused_relay(name: &str) -> (PathBuf, PathBuf, Child, std::process::ChildStdin) {
    let out = tmpdir(name);
    let pause = tmpdir(&format!("{name}-pause"));
    fs::create_dir_all(&pause).unwrap();
    let mut cmd = Command::new(RELAY);
    cmd.env("EVIDENCE_RELAY_TEST_PAUSE_DIR", &pause)
        .arg("--out")
        .arg(&out)
        .arg("--")
        .args(["/bin/sleep", "30"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut relay = cmd.spawn().expect("spawn relay");
    wait_ready(&out, 1, Duration::from_secs(5));
    let mut stdin = relay.stdin.take().unwrap();
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc":"2.0","id":"PAUSE_FRAME_1","method":"tools/call","params":{"name":"file_slice","arguments":{"path":"a"}}})
    )
    .unwrap();
    stdin.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pause.join("waiting").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        pause.join("waiting").exists(),
        "inbound thread reached the pause point"
    );
    (out, pause, relay, stdin)
}

fn file_lines(out: &Path) -> u64 {
    BufReader::new(fs::File::open(out.join("run-1.records.jsonl")).unwrap())
        .lines()
        .count() as u64
}

#[test]
fn f07_frame_in_flight_released_during_the_drain_is_included_before_the_recount() {
    let (out, pause, mut relay, _stdin) = spawn_paused_relay("inflight-drain");
    unsafe {
        libc::kill(relay.id() as libc::pid_t, libc::SIGTERM);
    }
    // Release while finalization drains admitted frames (within IN_FLIGHT_DRAIN_MS).
    fs::write(pause.join("release"), b"").unwrap();
    let _ = wait_exit(&mut relay, Duration::from_secs(5)).expect("relay exits");
    let m = manifest(&out, 1);
    let recs = records(&out, 1);
    assert_eq!(m["in_flight_unresolved"], 0, "{m}");
    assert_eq!(m["dropped_in_flight"], 0);
    assert_eq!(m["records"], m["records_recount"]);
    assert_eq!(
        m["records"].as_u64().unwrap(),
        file_lines(&out),
        "no append after recount"
    );
    assert_eq!(recs.len() as u64, m["records"].as_u64().unwrap());
    assert!(
        recs.iter()
            .any(|r| r["dir"] == "in" && r["method"] == "tools/call"),
        "the admitted frame was published before the recount: {recs:?}"
    );
}

#[test]
fn f07_frame_in_flight_held_past_the_drain_is_excluded_and_accounted_for() {
    let (out, pause, mut relay, _stdin) = spawn_paused_relay("inflight-held");
    let t0 = Instant::now();
    unsafe {
        libc::kill(relay.id() as libc::pid_t, libc::SIGTERM);
    }
    // Hold the thread past the drain window; finalization must still end within bounds.
    let status = wait_exit(&mut relay, Duration::from_secs(5)).expect("bounded termination");
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
    assert!(status.code().is_some() || status.signal().is_some());
    fs::write(pause.join("release"), b"").unwrap();
    let m = manifest(&out, 1);
    assert_eq!(m["complete"], false, "{m}");
    assert_eq!(m["reason"], "frame_in_flight");
    assert_eq!(m["in_flight_at_seal"], 1);
    assert_eq!(m["in_flight_unresolved"], 1);
    assert_eq!(m["records"], m["records_recount"]);
    assert_eq!(
        m["records"].as_u64().unwrap(),
        file_lines(&out),
        "no append after recount"
    );
    let text = fs::read_to_string(out.join("run-1.records.jsonl")).unwrap();
    assert!(
        !records(&out, 1).iter().any(|r| r["method"] == "tools/call"),
        "{text}"
    );
}
