//! Error-contract wire tests against the production binary.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{Value, json};

fn error_of(v: &Value) -> i64 {
    v["error"]["code"].as_i64().expect("error code")
}

#[test]
fn invalid_json_is_parse_error_without_id() {
    let mut s = Server::spawn();
    s.send_line("{not json");
    let e = s.recv_expect();
    assert_eq!(error_of(&e), -32700);
    assert!(e.get("id").is_none());
    s.send_line("");
    s.send(discover(json!(1), "2026-07-28"));
    assert_eq!(s.recv_expect()["result"]["resultType"], "complete");
}

#[test]
fn invalid_envelopes_are_invalid_request() {
    let mut s = Server::spawn();
    let cases: Vec<(String, Option<Value>)> = vec![
        ("[1,2]".into(), None),
        ("\"str\"".into(), None),
        (json!({"id":1,"method":"ping"}).to_string(), Some(json!(1))),
        (
            json!({"jsonrpc":"1.0","id":2,"method":"ping"}).to_string(),
            Some(json!(2)),
        ),
        (
            json!({"jsonrpc":"2.0","id":null,"method":"ping"}).to_string(),
            None,
        ),
        (
            json!({"jsonrpc":"2.0","id":1.5,"method":"ping"}).to_string(),
            None,
        ),
        (
            json!({"jsonrpc":"2.0","id":{"a":1},"method":"ping"}).to_string(),
            None,
        ),
        (
            json!({"jsonrpc":"2.0","id":3,"method":5}).to_string(),
            Some(json!(3)),
        ),
        (
            json!({"jsonrpc":"2.0","id":4,"method":"ping","params":[1]}).to_string(),
            Some(json!(4)),
        ),
        (
            json!({"jsonrpc":"2.0","id":5,"method":"ping","params":"x"}).to_string(),
            Some(json!(5)),
        ),
        (json!({"jsonrpc":"2.0","id":6}).to_string(), Some(json!(6))),
        (
            json!({"jsonrpc":"2.0","method":"nope/x","params":[1]}).to_string(),
            None,
        ),
    ];
    for (line, expected_id) in cases {
        s.send_line(&line);
        let e = s.recv_expect();
        assert_eq!(error_of(&e), -32600, "line {line}");
        match expected_id {
            Some(id) => assert_eq!(e["id"], id, "line {line}"),
            None => assert!(e.get("id").is_none(), "line {line} must not echo an id"),
        }
    }
}

#[test]
fn overlong_id_is_rejected_without_echo_and_254_char_id_is_accepted() {
    let mut s = Server::spawn();
    legacy_handshake(&mut s, "2025-11-25");
    let long = "q".repeat(300);
    s.send(json!({"jsonrpc":"2.0","id":long,"method":"ping"}));
    let raw = s.recv_raw(Duration::from_secs(5)).unwrap();
    let e: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(error_of(&e), -32600);
    assert!(e.get("id").is_none());
    assert!(
        !String::from_utf8_lossy(&raw).contains("qqqqqqqq"),
        "oversized id must not be echoed"
    );
    let ok = "r".repeat(254);
    s.send(json!({"jsonrpc":"2.0","id":ok,"method":"ping"}));
    let r = s.recv_expect();
    assert_eq!(r["id"], ok);
    assert_eq!(r["result"], json!({}));
    s.send(json!({"jsonrpc":"2.0","id":9007199254740993i64,"method":"ping"}));
    assert_eq!(s.recv_expect()["id"], 9007199254740993i64);
}

#[test]
fn known_method_with_malformed_params_is_invalid_params() {
    let mut s = Server::spawn();
    legacy_handshake(&mut s, "2025-11-25");
    s.send(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":5}}));
    let e = s.recv_expect();
    assert_eq!(error_of(&e), -32602);
    assert_eq!(e["id"], 7);
    s.send(json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"nope"}}));
    let e = s.recv_expect();
    assert_eq!(error_of(&e), -32602);
    assert_eq!(e["error"]["message"], "tool not found");
}

#[test]
fn argument_failures_are_compact_tool_errors() {
    let mut s = Server::spawn();
    legacy_handshake(&mut s, "2025-11-25");
    let cases: Vec<(Value, &str)> = vec![
        (json!({"name":"connection_probe"}), "missing nonce"),
        (
            json!({"name":"connection_probe","arguments":{}}),
            "missing nonce",
        ),
        (
            json!({"name":"connection_probe","arguments":{"nonce":null}}),
            "invalid nonce",
        ),
        (
            json!({"name":"connection_probe","arguments":{"nonce":5}}),
            "invalid nonce",
        ),
        (
            json!({"name":"connection_probe","arguments":{"nonce":""}}),
            "invalid nonce",
        ),
        (
            json!({"name":"connection_probe","arguments":{"nonce":"a".repeat(65)}}),
            "invalid nonce",
        ),
        (
            json!({"name":"connection_probe","arguments":{"nonce":"caf\u{e9}"}}),
            "invalid nonce",
        ),
        (
            json!({"name":"connection_probe","arguments":{"nonce":"a-b"}}),
            "invalid nonce",
        ),
        (
            json!({"name":"connection_probe","arguments":{"nonce":"a b"}}),
            "invalid nonce",
        ),
        (
            json!({"name":"connection_probe","arguments":{"nonce":"ok","extra":1}}),
            "unexpected argument",
        ),
        (
            json!({"name":"connection_probe","arguments":{"nonce":"ok","pad":"x".repeat(5000)}}),
            "arguments too large",
        ),
    ];
    for (i, (params, expected)) in cases.into_iter().enumerate() {
        s.send(json!({"jsonrpc":"2.0","id":i,"method":"tools/call","params":params}));
        let r = s.recv_expect();
        assert_eq!(r["id"], i);
        assert_eq!(r["result"]["isError"], true, "case {i}");
        assert_eq!(r["result"]["content"][0]["text"], expected, "case {i}");
        assert!(r["result"].get("structuredContent").is_none(), "case {i}");
        assert!(compact(&r["result"]).len() <= 1024);
    }
}

#[test]
fn valid_notifications_and_client_responses_get_no_reply() {
    let mut s = Server::spawn();
    legacy_handshake(&mut s, "2025-11-25");
    s.send(json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":1,"progress":1}}));
    s.send(json!({"jsonrpc":"2.0","method":"notifications/roots/list_changed"}));
    s.send(json!({"jsonrpc":"2.0","id":99,"result":{}}));
    s.send(json!({"jsonrpc":"2.0","id":98,"error":{"code":-1,"message":"x"}}));
    s.send(cancel(json!(12345)));
    s.send(json!({"jsonrpc":"2.0","id":2,"method":"ping"}));
    let first = s.recv_expect();
    assert_eq!(first["id"], 2, "the only reply is the ping result");
    s.assert_silent(Duration::from_millis(300));
}

#[test]
fn oversized_frame_terminates_with_code_3() {
    let mut s = Server::spawn();
    let big = vec![b'a'; 70_000];
    s.send_raw(&big);
    let status = s
        .wait_exit(Duration::from_secs(5))
        .expect("server exits on oversized frame");
    assert_eq!(exit_code(status), 3);
    let stderr = s.stderr_after_exit();
    assert!(
        stderr.iter().any(|l| l.contains("event=frame.oversized")),
        "{stderr:#?}"
    );
    assert!(
        stderr
            .iter()
            .any(|l| l.contains("event=fatal cause=frame_oversized code=3")),
        "{stderr:#?}"
    );
}

#[test]
fn oversized_frame_with_newline_also_terminates() {
    let mut s = Server::spawn();
    let mut big = vec![b'b'; 65_536];
    big.push(b'\n');
    s.send_raw(&big);
    assert_eq!(exit_code(s.wait_exit(Duration::from_secs(5)).unwrap()), 3);
}

#[test]
fn exact_limit_frame_is_accepted() {
    let mut s = Server::spawn();
    legacy_handshake(&mut s, "2025-11-25");
    // Build a frame of exactly 65,536 bytes including the newline: a ping with padded params.
    let base = json!({"jsonrpc":"2.0","id":1,"method":"ping","params":{"pad":""}}).to_string();
    let pad = 65_536 - 1 - base.len();
    let line = json!({"jsonrpc":"2.0","id":1,"method":"ping","params":{"pad":"p".repeat(pad)}})
        .to_string();
    assert_eq!(line.len() + 1, 65_536);
    s.send_line(&line);
    assert_eq!(s.recv_expect()["id"], 1);
}

fn ping_with_version(id: Value, version: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"ping","params":{"_meta":{
        "io.modelcontextprotocol/protocolVersion": version,
        "io.modelcontextprotocol/clientCapabilities": {}}}})
}

/// The process must end with the protocol-violation code, nothing may have reached stdout, and the
/// cause must be the outbound bound, not the inbound frame bound.
fn assert_outbound_too_large(s: &mut Server) {
    let status = s
        .wait_exit(Duration::from_secs(5))
        .expect("server exits on an oversized outbound frame");
    assert_eq!(exit_code(status), 3);
    assert!(
        s.recv_raw(Duration::from_millis(200)).is_none(),
        "no frame may reach stdout after the oversized error"
    );
    let stderr = s.stderr_after_exit();
    assert!(
        stderr
            .iter()
            .any(|l| l.contains("event=outbound.oversized bytes=")),
        "{stderr:#?}"
    );
    assert!(
        stderr
            .iter()
            .any(|l| l.contains("event=fatal cause=outbound_too_large code=3")),
        "{stderr:#?}"
    );
    assert!(
        !stderr.iter().any(|l| l.contains("frame_oversized")),
        "the inbound frame bound must not be what fired: {stderr:#?}"
    );
}

#[test]
fn long_unsupported_version_is_never_echoed_and_terminates_with_code_3() {
    // TS-M0-CODE-REVIEW-002 F06 reproduction: ping, id 71, version of 20,000 characters.
    let mut s = Server::spawn();
    let frame = ping_with_version(json!(71), &"X".repeat(20_000)).to_string();
    assert!(
        frame.len() < 65_536,
        "the inbound frame itself is within bounds"
    );
    s.send_line(&frame);
    assert_outbound_too_large(&mut s);
    assert!(s.all_stdout_raw().is_empty());
}

#[test]
fn unsupported_version_with_escaped_characters_is_bounded_by_serialized_bytes() {
    // Each repeat is four characters that serialize to twelve bytes: the raw length stays far
    // below the bound while the serialized error frame does not.
    let version = "\"\\\u{1}\n".repeat(1_500);
    assert!(version.chars().count() < limits_outbound());
    let mut s = Server::spawn();
    let frame = ping_with_version(json!("esc-71"), &version).to_string();
    assert!(
        frame.len() < 65_536,
        "the inbound frame itself is within bounds"
    );
    s.send_line(&frame);
    assert_outbound_too_large(&mut s);
}

fn limits_outbound() -> usize {
    16_384
}

#[test]
fn unsupported_version_error_at_the_outbound_limit_is_delivered_and_one_byte_over_is_fatal() {
    let mut s = Server::spawn();
    s.send(ping_with_version(json!(1), &"v".repeat(100)));
    let probe = s.recv_raw(Duration::from_secs(5)).expect("version error");
    let e: Value = serde_json::from_slice(&probe).unwrap();
    assert_eq!(error_of(&e), -32022);
    assert_eq!(e["error"]["data"]["requested"], "v".repeat(100));
    // Grow the version so the serialized error frame, newline included, is exactly the bound.
    let fit = 100 + (limits_outbound() - probe.len());
    s.send(ping_with_version(json!(2), &"v".repeat(fit)));
    let exact = s
        .recv_raw(Duration::from_secs(5))
        .expect("an error frame at the exact bound is delivered");
    assert_eq!(exact.len(), limits_outbound());
    let e: Value = serde_json::from_slice(&exact).unwrap();
    assert_eq!(error_of(&e), -32022);
    assert_eq!(e["id"], 2);
    // Delivered traffic in between does not change the rule.
    s.send(discover(json!(3), "2026-07-28"));
    assert_eq!(s.recv_expect()["result"]["resultType"], "complete");
    s.send(ping_with_version(json!(4), &"v".repeat(fit + 1)));
    assert_outbound_too_large(&mut s);
}
