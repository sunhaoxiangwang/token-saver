//! Lifecycle, isolation, diagnostics, and CLI tests against the production binary.

mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use common::*;
use serde_json::json;

#[test]
fn eof_exits_zero_promptly() {
    let mut s = Server::spawn();
    assert!(s.startup_instance_id().is_some());
    let t0 = Instant::now();
    s.close_stdin();
    let status = s.wait_exit(Duration::from_secs(2)).expect("exit");
    let elapsed = t0.elapsed();
    assert_eq!(exit_code(status), 0);
    assert!(
        elapsed < Duration::from_millis(500),
        "EOF to exit took {elapsed:?}"
    );
}

#[test]
fn eof_immediately_after_a_request_still_delivers_the_response_then_exits_zero() {
    for _ in 0..5 {
        let mut s = Server::spawn();
        s.send(discover(json!(1), "2026-07-28"));
        s.close_stdin();
        let resp = s
            .recv(Duration::from_secs(5))
            .expect("response delivered after EOF");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["resultType"], "complete");
        let status = s.wait_exit(Duration::from_secs(4)).expect("exit");
        assert_eq!(exit_code(status), 0);
        let stderr = s.stderr_after_exit();
        assert!(
            stderr
                .iter()
                .any(|l| l.contains("event=shutdown trigger=eof in_flight=0 code=0")),
            "{stderr:#?}"
        );
    }
}

#[test]
fn eof_after_a_burst_of_accepted_requests_delivers_every_response_then_exits_zero() {
    let mut s = Server::spawn();
    let mut burst = initialize(json!(0), "2025-11-25").to_string();
    burst.push('\n');
    burst.push_str(&initialized().to_string());
    burst.push('\n');
    for i in 1..=8 {
        burst.push_str(&probe_call(json!(i), &format!("n{i}"), false).to_string());
        burst.push('\n');
    }
    s.send_raw(burst.as_bytes());
    s.close_stdin();
    let mut ids = Vec::new();
    for _ in 0..9 {
        ids.push(
            s.recv(Duration::from_secs(5))
                .expect("all responses delivered")["id"]
                .as_u64()
                .unwrap(),
        );
    }
    ids.sort();
    assert_eq!(ids, (0..=8).collect::<Vec<_>>());
    assert_eq!(exit_code(s.wait_exit(Duration::from_secs(4)).unwrap()), 0);
}

#[test]
fn eof_after_work_exits_zero() {
    let mut s = Server::spawn();
    legacy_handshake(&mut s, "2025-11-25");
    s.send(probe_call(json!(2), "n", false));
    s.recv_expect();
    s.close_stdin();
    assert_eq!(exit_code(s.wait_exit(Duration::from_secs(2)).unwrap()), 0);
}

#[test]
fn sigint_and_sigterm_exit_zero_promptly() {
    for sig in [libc::SIGINT, libc::SIGTERM] {
        let mut s = Server::spawn();
        assert!(s.startup_instance_id().is_some());
        legacy_handshake(&mut s, "2025-11-25");
        let t0 = Instant::now();
        s.signal(sig);
        let status = s.wait_exit(Duration::from_secs(2)).expect("exit on signal");
        assert_eq!(exit_code(status), 0, "signal {sig}");
        assert!(t0.elapsed() < Duration::from_millis(500));
    }
}

#[test]
fn stdout_carries_only_json_rpc_objects() {
    let mut s = Server::spawn();
    legacy_handshake(&mut s, "2025-11-25");
    s.send_line("{garbage");
    s.send(probe_call(json!(2), "n", false));
    s.send(json!({"jsonrpc":"2.0","id":3,"method":"nope"}));
    for _ in 0..3 {
        s.recv_expect();
    }
    s.close_stdin();
    s.wait_exit(Duration::from_secs(2));
    for line in s.all_stdout_raw() {
        let v: serde_json::Value = serde_json::from_slice(line).expect("JSON");
        assert_eq!(v["jsonrpc"], "2.0");
        assert!(v.is_object());
    }
}

#[test]
fn environment_never_appears_in_output() {
    let canary_dir = "/tmp/claude-project-canary-7e1f";
    let canary = "ENVCANARY_5d9a3c";
    let mut s = Server::spawn_with(
        &["serve", "--diag", "debug"],
        &[
            ("CLAUDE_PROJECT_DIR", canary_dir),
            ("TS_CANARY", canary),
            ("CONTROL_PLANE_API_KEY", "sk-canary-0000"),
        ],
    );
    legacy_handshake(&mut s, "2025-11-25");
    s.send(tools_list(json!(2), false));
    s.recv_expect();
    s.send(probe_call(json!(3), "abc", false));
    s.recv_expect();
    s.close_stdin();
    s.wait_exit(Duration::from_secs(2));
    let stdout: String = s
        .all_stdout_raw()
        .iter()
        .map(|l| String::from_utf8_lossy(l).to_string())
        .collect();
    let stderr = s.stderr_after_exit().join("\n");
    for needle in [canary_dir, canary, "sk-canary"] {
        assert!(!stdout.contains(needle), "stdout leaked {needle}");
        assert!(!stderr.contains(needle), "stderr leaked {needle}");
    }
}

#[test]
fn canaries_are_absent_from_diagnostics_at_every_level() {
    let canary = "CANARY_ZQ77";
    for level in ["off", "error", "info", "debug"] {
        let mut s = Server::spawn_with(&["serve", "--diag", level], &[]);
        // malformed frame carrying the canary
        s.send_line(&format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":\"{canary}a\",\"method\":"
        ));
        // client identity in _meta
        s.send(json!({"jsonrpc":"2.0","id":"i1","method":"server/discover","params":{"_meta":{
            "io.modelcontextprotocol/protocolVersion":"2026-07-28",
            "io.modelcontextprotocol/clientInfo":{"name":format!("{canary}b"),"version":format!("{canary}c")},
            "io.modelcontextprotocol/clientCapabilities":{}}}}));
        // canary in a request id, an unknown method name, and an argument value
        s.send(json!({"jsonrpc":"2.0","id":format!("{canary}d"),"method":"ping","params":{"_meta":meta("2026-07-28")}}));
        s.send(json!({"jsonrpc":"2.0","id":"m1","method":format!("x/{canary}e"),"params":{"_meta":meta("2026-07-28")}}));
        s.send(tools_call(
            json!("c1"),
            json!({"nonce":format!("{canary}f!")}),
            true,
        ));
        s.send(tools_call(
            json!("c2"),
            json!({"nonce":"ok", format!("{canary}g"): 1}),
            true,
        ));
        // canary in a cancellation reason and an unknown-notification payload
        s.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"zz","reason":format!("{canary}h")}}));
        s.send(json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":format!("{canary}i"),"progress":1}}));
        for _ in 0..6 {
            s.recv(Duration::from_secs(5)).expect("reply");
        }
        s.close_stdin();
        s.wait_exit(Duration::from_secs(2));
        let stderr = s.stderr_after_exit();
        for line in &stderr {
            assert!(
                !line.contains(canary),
                "level {level} leaked canary in diagnostics: {line}"
            );
        }
        if level == "off" {
            assert!(
                stderr.iter().all(|l| l.contains("event=startup")),
                "off emits only startup: {stderr:#?}"
            );
        }
    }
}

#[test]
fn cli_version_and_usage_errors() {
    let out = Command::new(BIN).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("token-saver {}", env!("CARGO_PKG_VERSION"))
    );
    for args in [
        vec![],
        vec!["serve", "--bogus"],
        vec!["serve", "--diag", "loud"],
        vec!["serve", "--capture", "x"],
        vec!["--test-hold-fifo", "/tmp/x"],
    ] {
        let out = Command::new(BIN).args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "args {args:?}");
        assert!(out.stdout.is_empty(), "usage errors never write stdout");
    }
}

#[test]
fn no_application_files_or_network_listeners_are_open() {
    let mut s = Server::spawn();
    legacy_handshake(&mut s, "2025-11-25");
    s.send(probe_call(json!(2), "n", false));
    s.recv_expect();
    let out = match Command::new("lsof")
        .args(["-p", &s.pid().to_string()])
        .output()
    {
        Ok(o) if o.status.success() || !o.stdout.is_empty() => o,
        _ => {
            eprintln!("SKIPPED: lsof unavailable");
            return;
        }
    };
    let text = String::from_utf8_lossy(&out.stdout);
    // Unix socketpairs whose both ends belong to this process are tokio's internal wakeup and
    // signal plumbing, not listeners. Collect our own socket addresses first.
    let mut own_unix_addrs = std::collections::HashSet::new();
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 6 && cols[4] == "unix" {
            own_unix_addrs.insert(cols[5].to_owned());
        }
    }
    let mut offending = Vec::new();
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 5 {
            continue;
        }
        let kind = cols[4];
        let name = cols.last().copied().unwrap_or("");
        let runtime_file = name.ends_with("/token-saver")
            || name.starts_with("/usr/lib")
            || name.starts_with("/System/")
            || name.starts_with("/Library/")
            || name.starts_with("/private/var/db/")
            || name.starts_with("/dev/")
            || name == "/"
            || name.contains("token-saver/target/");
        match kind {
            "REG" if !runtime_file => offending.push(line.to_owned()),
            "IPv4" | "IPv6" => offending.push(line.to_owned()),
            "unix" => {
                let peer_internal = name
                    .strip_prefix("->")
                    .is_some_and(|peer| own_unix_addrs.contains(peer));
                if !peer_internal {
                    offending.push(line.to_owned());
                }
            }
            _ => {}
        }
    }
    assert!(
        offending.is_empty(),
        "unexpected descriptors: {offending:#?}"
    );
}
