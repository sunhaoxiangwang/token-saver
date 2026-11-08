//! Release-binary wire tests with `--root`: the four-tool listing and its golden, both eras,
//! error texts, real FIFOs and sockets (C9, C11), large inputs within budget, concurrency without
//! `busy`, orderly exit, flag errors, and the descriptor baseline (S8).

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::ws::TempRoot;
use common::*;
use serde_json::{Value, json};

fn spawn_root(root: &TempRoot) -> Server {
    Server::spawn_with(
        &[
            "serve",
            "--diag",
            "debug",
            "--root",
            root.path.to_str().unwrap(),
        ],
        &[],
    )
}

fn ws_call(id: Value, tool: &str, arguments: Value, modern: bool) -> Value {
    let mut params = json!({"name": tool, "arguments": arguments});
    if modern {
        params["_meta"] = meta("2026-07-28");
    }
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":params})
}

fn text(v: &Value) -> String {
    common::ws::content_of(v["result"]["content"][0]["text"].as_str().unwrap())
}

fn result_meta(v: &Value) -> Value {
    assert!(
        v["result"].get("structuredContent").is_none(),
        "text-only results: {v}"
    );
    common::ws::meta_of(v["result"]["content"][0]["text"].as_str().unwrap())
}

fn fixture_root() -> TempRoot {
    let root = TempRoot::new("wire");
    root.write("README.md", b"# demo\nneedle in the readme\n");
    root.write(
        "src/main.rs",
        b"fn main() {}\nconst MAX_OUTBOUND_FRAME_BYTES: usize = 16_384;\n",
    );
    root.write(".env", b"SECRET=1\n");
    root.mkdir("target/debug");
    root.write("target/debug/junk", b"needle\n");
    root
}

#[test]
fn root_lists_four_tools_and_matches_golden() {
    let root = fixture_root();
    let mut s = spawn_root(&root);
    s.send(discover(json!(0), "2026-07-28"));
    let d = s.recv_expect();
    let instructions = d["result"]["instructions"].as_str().unwrap();
    assert!(instructions.len() <= 512);
    assert!(instructions.contains("workspace_outline") && instructions.contains("file_slice"));
    s.send(tools_list(json!(1), true));
    let list = s.recv_expect();
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "connection_probe",
            "workspace_outline",
            "file_slice",
            "text_search"
        ]
    );
    assert_eq!(list["result"]["ttlMs"], 0);
    assert_eq!(list["result"]["cacheScope"], "private");
    for t in tools {
        assert_eq!(
            t["annotations"],
            json!({"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false})
        );
        assert!(t["description"].as_str().unwrap().len() <= 1024);
    }
    let pack = Value::Array(tools[1..].to_vec());
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tools.root.golden.json");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, serde_json::to_string_pretty(&pack).unwrap() + "\n").unwrap();
    }
    let golden: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("golden present")).unwrap();
    assert_eq!(
        pack, golden,
        "workspace tool definitions changed; review and set UPDATE_GOLDEN=1"
    );
    let lines = s.stderr_lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("event=workspace.root workers=4 slots=8"))
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.contains(root.path.to_str().unwrap())),
        "root path must never appear in diagnostics"
    );
}

#[test]
fn without_root_exactly_the_probe_is_listed() {
    let mut s = Server::spawn();
    s.send(tools_list(json!(1), true));
    let list = s.recv_expect();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 1);
    s.send(ws_call(json!(2), "file_slice", json!({"path":"x"}), true));
    let e = s.recv_expect();
    assert_eq!(e["error"]["code"], -32602);
    assert_eq!(e["error"]["message"], "tool not found");
}

#[test]
fn tools_work_in_both_eras() {
    let root = fixture_root();
    let mut legacy = spawn_root(&root);
    legacy_handshake(&mut legacy, "2025-11-25");
    legacy.send(tools_list(json!(2), false));
    assert_eq!(
        legacy.recv_expect()["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    legacy.send(ws_call(json!(3), "workspace_outline", json!({}), false));
    let o = legacy.recv_expect();
    assert!(o["result"].get("resultType").is_none());
    assert!(text(&o).contains("f README.md 28"));
    legacy.send(ws_call(
        json!(4),
        "file_slice",
        json!({"path":"src/main.rs"}),
        false,
    ));
    assert_eq!(
        text(&legacy.recv_expect()).lines().next().unwrap(),
        "1\tfn main() {}"
    );
    legacy.send(ws_call(
        json!(5),
        "text_search",
        json!({"query":"needle"}),
        false,
    ));
    let r = legacy.recv_expect();
    assert_eq!(text(&r), "== README.md\n2:needle in the readme");
    assert_eq!(result_meta(&r)["files_skipped"]["excluded"], 1);
    assert_eq!(result_meta(&r)["files_skipped"]["hidden"], 1);

    let mut modern = spawn_root(&root);
    modern.send(ws_call(
        json!(1),
        "workspace_outline",
        json!({"include_hidden":true}),
        true,
    ));
    let o = modern.recv_expect();
    assert_eq!(o["result"]["resultType"], "complete");
    assert!(text(&o).contains("f .env denied"));
    modern.send(ws_call(
        json!(2),
        "file_slice",
        json!({"path":".env"}),
        true,
    ));
    let d = modern.recv_expect();
    assert_eq!(d["result"]["isError"], true);
    assert_eq!(text(&d), "denied path");
    modern.send(ws_call(
        json!(3),
        "text_search",
        json!({"query":"MAX_OUTBOUND","path":"src"}),
        true,
    ));
    let r = modern.recv_expect();
    assert_eq!(
        text(&r),
        "== src/main.rs\n2:const MAX_OUTBOUND_FRAME_BYTES: usize = 16_384;"
    );
    modern.send(probe_call(json!(4), "still", true));
    assert_eq!(
        modern.recv_expect()["result"]["structuredContent"]["nonce"],
        "still"
    );
}

#[test]
fn error_texts_over_the_wire() {
    let root = fixture_root();
    let mut s = spawn_root(&root);
    let cases: Vec<(&str, Value, &str)> = vec![
        ("file_slice", json!({}), "missing path"),
        ("file_slice", json!({"path":"../x"}), "invalid path"),
        ("file_slice", json!({"path":"missing.rs"}), "not found"),
        ("file_slice", json!({"path":"src"}), "not a file"),
        (
            "file_slice",
            json!({"path":"README.md/x"}),
            "not a directory",
        ),
        ("file_slice", json!({"path":".env"}), "denied path"),
        (
            "file_slice",
            json!({"path":"src/main.rs","max_lines":0}),
            "invalid argument",
        ),
        (
            "file_slice",
            json!({"path":"src/main.rs","nope":1}),
            "unexpected argument",
        ),
        (
            "file_slice",
            json!({"path":"src/main.rs","big":"x".repeat(4_100)}),
            "arguments too large",
        ),
        (
            "workspace_outline",
            json!({"path":"README.md"}),
            "not a directory",
        ),
        ("text_search", json!({"query":""}), "invalid argument"),
        (
            "text_search",
            json!({"query":"x","path":"nowhere"}),
            "not found",
        ),
    ];
    for (i, (tool, args, expected)) in cases.into_iter().enumerate() {
        s.send(ws_call(json!(i + 1), tool, args.clone(), true));
        let r = s.recv_expect();
        assert_eq!(r["result"]["isError"], true, "{tool} {args}: {r}");
        assert_eq!(text(&r), expected, "{tool} {args}");
    }
    s.send(json!({"jsonrpc":"2.0","id":99,"method":"tools/call","params":{"_meta":meta("2026-07-28"),"name":"nope","arguments":{}}}));
    let e = s.recv_expect();
    assert_eq!(e["error"]["code"], -32602);
    assert_eq!(e["error"]["message"], "tool not found");
}

#[test]
fn fifo_and_socket_on_the_real_filesystem() {
    let root = TempRoot::new("fs");
    root.write("t.txt", b"needle\n");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(root.path.join("pipe"))
            .status()
            .unwrap()
            .success()
    );
    let _listener = std::os::unix::net::UnixListener::bind(root.path.join("sock")).unwrap();
    let mut s = spawn_root(&root);
    let t0 = Instant::now();
    s.send(ws_call(
        json!(1),
        "file_slice",
        json!({"path":"pipe"}),
        true,
    ));
    assert_eq!(text(&s.recv_expect()), "not a file");
    s.send(ws_call(
        json!(2),
        "file_slice",
        json!({"path":"sock"}),
        true,
    ));
    assert_eq!(text(&s.recv_expect()), "not a file");
    assert!(t0.elapsed() < Duration::from_millis(500));
    s.send(ws_call(json!(3), "workspace_outline", json!({}), true));
    assert_eq!(text(&s.recv_expect()), "o pipe\no sock\nf t.txt 7");
    s.send(ws_call(
        json!(4),
        "text_search",
        json!({"query":"needle"}),
        true,
    ));
    let r = s.recv_expect();
    assert_eq!(text(&r), "== t.txt\n1:needle");
    assert_eq!(result_meta(&r)["files_skipped"]["other"], 2);
}

#[test]
fn large_inputs_complete_within_budget_and_the_server_exits_0_at_eof() {
    let root = TempRoot::new("big");
    let mut big = String::new();
    for i in 0..20_000 {
        big.push_str(&format!("line {i:05} {}\n", "x".repeat(40)));
    }
    assert!(big.len() > 1_000_000);
    root.write("big.txt", big.as_bytes());
    for i in 0..5_000 {
        root.write(&format!("many/e{i:04}"), b"");
    }
    let mut s = spawn_root(&root);
    let t0 = Instant::now();
    s.send(ws_call(
        json!(1),
        "file_slice",
        json!({"path":"big.txt","start_line":19_990,"max_lines":20}),
        true,
    ));
    let r = s.recv_expect();
    assert_eq!(result_meta(&r)["returned_lines"], 11);
    assert_eq!(result_meta(&r)["at_eof"], true);
    assert_eq!(result_meta(&r)["stop_reason"], "complete");
    assert_eq!(result_meta(&r)["total_lines"], 20_000);
    s.send(ws_call(
        json!(2),
        "workspace_outline",
        json!({"path":"many","max_entries":200}),
        true,
    ));
    let o = s.recv_expect();
    assert_eq!(result_meta(&o)["entries_listed"], 200);
    assert_eq!(result_meta(&o)["stop_reason"], "max_entries");
    s.send(ws_call(
        json!(3),
        "text_search",
        json!({"query":"line 19999"}),
        true,
    ));
    let f = s.recv_expect();
    assert_eq!(result_meta(&f)["matches"], 1);
    assert!(t0.elapsed() < Duration::from_secs(4));
    for i in 10..18 {
        s.send(ws_call(
            json!(i),
            "file_slice",
            json!({"path":"big.txt","start_line": i * 100}),
            true,
        ));
    }
    for _ in 0..8 {
        let r = s.recv_expect();
        assert_eq!(r["result"]["isError"], false, "{r}");
    }
    // The burst above exceeds the 64-events-per-second diagnostics budget; let the window reset
    // so the shutdown line is not dropped (drops make diagnostic assertions inconclusive).
    std::thread::sleep(Duration::from_millis(1_100));
    s.close_stdin();
    let status = s.wait_exit(Duration::from_secs(5)).expect("exit");
    assert_eq!(exit_code(status), 0);
    let lines = s.stderr_after_exit();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("event=shutdown trigger=eof in_flight=0 code=0")),
        "{lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("worker_stall") || l.contains("job.busy"))
    );
    assert!(
        lines
            .iter()
            .filter(|l| l.contains("event=job.finished"))
            .count()
            >= 8
    );
}

#[test]
fn root_flag_errors_exit_2() {
    let root = TempRoot::new("flags");
    let link = root.outside().join("rootlink");
    std::os::unix::fs::symlink(&root.path, &link).unwrap();
    for args in [
        vec!["serve", "--root", "relative/dir"],
        vec!["serve", "--root"],
        vec!["serve", "--root", "/nonexistent/dir/for/token-saver"],
        vec!["serve", "--root", link.to_str().unwrap()],
        vec![
            "serve",
            "--root",
            root.path.to_str().unwrap(),
            "--root",
            root.path.to_str().unwrap(),
        ],
    ] {
        let mut s = Server::spawn_with(&args, &[]);
        let status = s.wait_exit(Duration::from_secs(5)).expect("exit");
        assert_eq!(exit_code(status), 2, "{args:?}");
    }
    let file = root.path.join("afile");
    std::fs::write(&file, b"x").unwrap();
    let mut s = Server::spawn_with(&["serve", "--root", file.to_str().unwrap()], &[]);
    let status = s.wait_exit(Duration::from_secs(5)).expect("exit");
    assert_eq!(exit_code(status), 2);
    let lines = s.stderr_after_exit();
    assert!(
        lines.iter().any(|l| l.contains("event=root.refused")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains(file.to_str().unwrap())),
        "the refused path must not appear"
    );
}

#[test]
fn s8_lsof_descriptor_baseline_is_unchanged_after_work() {
    let root = fixture_root();
    for i in 0..30 {
        root.write(&format!("d/f{i:02}.txt"), b"needle\n");
    }
    let mut s = spawn_root(&root);
    s.send(tools_list(json!(0), true));
    s.recv_expect();
    let count = |pid: u32| -> usize {
        let out = std::process::Command::new("lsof")
            .args(["-p", &pid.to_string()])
            .output()
            .expect("lsof");
        String::from_utf8_lossy(&out.stdout).lines().count()
    };
    std::thread::sleep(Duration::from_millis(200));
    let before = count(s.pid());
    for i in 1..=20 {
        s.send(ws_call(
            json!(i),
            "file_slice",
            json!({"path": format!("d/f{:02}.txt", i % 30)}),
            true,
        ));
        s.recv_expect();
    }
    for i in 21..=25 {
        s.send(ws_call(
            json!(i),
            "workspace_outline",
            json!({"depth":3}),
            true,
        ));
        s.recv_expect();
        s.send(ws_call(
            json!(100 + i),
            "text_search",
            json!({"query":"needle"}),
            true,
        ));
        s.recv_expect();
    }
    std::thread::sleep(Duration::from_millis(200));
    let after = count(s.pid());
    assert_eq!(
        before, after,
        "descriptor count changed: {before} -> {after}"
    );
    assert!(before > 0);
}

#[test]
fn review_f06_bare_wildcard_prefix_is_denied_by_every_tool() {
    let root = TempRoot::new("deny");
    for name in [".env.", ".env.local", "id_rsa.", "credentials.", "k.key"] {
        root.write(name, b"SECRET_VALUE_7\n");
    }
    root.write("ok.txt", b"SECRET_VALUE_7 is not here\n");
    let mut s = spawn_root(&root);
    for (i, name) in [".env.", ".env.local", "id_rsa.", "credentials.", "k.key"]
        .iter()
        .enumerate()
    {
        s.send(ws_call(
            json!(i + 1),
            "file_slice",
            json!({"path": name}),
            true,
        ));
        let r = s.recv_expect();
        assert_eq!(text(&r), "denied path", "{name}");
    }
    s.send(ws_call(
        json!(10),
        "workspace_outline",
        json!({"include_hidden": true}),
        true,
    ));
    let o = text(&s.recv_expect());
    assert!(
        o.contains("f .env. denied")
            && o.contains("f id_rsa. denied")
            && o.contains("f credentials. denied"),
        "{o}"
    );
    assert!(!o.contains("SECRET_VALUE_7"));
    s.send(ws_call(
        json!(11),
        "text_search",
        json!({"query":"SECRET_VALUE_7","include_hidden":true}),
        true,
    ));
    let r = s.recv_expect();
    assert_eq!(text(&r), "== ok.txt\n1:SECRET_VALUE_7 is not here");
    assert_eq!(result_meta(&r)["files_skipped"]["denied"], 5);
}
