//! Modern (2026-07-28 per-request metadata) wire tests against the production binary.

mod common;

use common::*;
use serde_json::json;

#[test]
fn discover_advertises_exactly_two_versions() {
    let mut s = Server::spawn();
    s.send(discover(json!("d1"), "2026-07-28"));
    let d = s.recv_expect();
    assert_eq!(d["id"], "d1");
    let r = &d["result"];
    assert_eq!(r["resultType"], "complete");
    assert_eq!(r["supportedVersions"], json!(["2025-11-25", "2026-07-28"]));
    assert!(r["capabilities"]["tools"].is_object());
    assert_eq!(
        r["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "token-saver"
    );
    assert!(r.get("ttlMs").is_some());
    assert!(r.get("cacheScope").is_some());
    assert!(r["instructions"].as_str().unwrap().len() <= 512);
}

#[test]
fn modern_list_and_call_carry_result_type_and_cache_hints() {
    let mut s = Server::spawn();
    s.send(discover(json!(1), "2026-07-28"));
    s.recv_expect();
    s.send(tools_list(json!(2), true));
    let list = s.recv_expect();
    assert_eq!(list["result"]["resultType"], "complete");
    assert!(list["result"].get("ttlMs").is_some());
    assert!(list["result"].get("cacheScope").is_some());
    assert_eq!(list["result"]["tools"][0]["name"], "connection_probe");
    s.send(probe_call(json!(3), "XyZ9", true));
    let call = s.recv_expect();
    assert_eq!(call["result"]["resultType"], "complete");
    assert_eq!(call["result"]["structuredContent"]["nonce"], "XyZ9");
    assert_eq!(call["result"]["isError"], false);
}

#[test]
fn modern_call_works_without_prior_discover() {
    let mut s = Server::spawn();
    s.send(probe_call(json!("only"), "fresh1", true));
    let call = s.recv_expect();
    assert_eq!(call["id"], "only");
    assert_eq!(call["result"]["structuredContent"]["nonce"], "fresh1");
}

#[test]
fn modern_ping_is_method_not_found_at_every_lifecycle_stage() {
    let mut s = Server::spawn();
    // before any lifecycle is selected
    s.send(json!({"jsonrpc":"2.0","id":0,"method":"ping","params":{"_meta":meta("2026-07-28")}}));
    assert_eq!(s.recv_expect()["error"]["code"], -32601);
    // after discover
    s.send(discover(json!(1), "2026-07-28"));
    s.recv_expect();
    s.send(json!({"jsonrpc":"2.0","id":2,"method":"ping","params":{"_meta":meta("2026-07-28")}}));
    assert_eq!(s.recv_expect()["error"]["code"], -32601);
    // after a tool invocation
    s.send(probe_call(json!(3), "n", true));
    s.recv_expect();
    s.send(json!({"jsonrpc":"2.0","id":4,"method":"ping","params":{"_meta":meta("2026-07-28")}}));
    let e = s.recv_expect();
    assert_eq!(e["error"]["code"], -32601);
    assert_eq!(e["id"], 4);
}

#[test]
fn legacy_ping_is_answered_before_and_after_initialize() {
    let mut s = Server::spawn();
    s.send(json!({"jsonrpc":"2.0","id":0,"method":"ping"}));
    assert_eq!(s.recv_expect()["result"], json!({}));
    legacy_handshake(&mut s, "2025-11-25");
    s.send(json!({"jsonrpc":"2.0","id":2,"method":"ping"}));
    assert_eq!(s.recv_expect()["result"], json!({}));
    // a ping carrying legacy metadata is still a legacy ping
    s.send(json!({"jsonrpc":"2.0","id":3,"method":"ping","params":{"_meta":meta("2025-11-25")}}));
    assert_eq!(s.recv_expect()["result"], json!({}));
}

#[test]
fn unsupported_version_is_rejected_at_every_lifecycle_stage_and_for_every_method() {
    let mut s = Server::spawn();
    for (id, method) in [
        (0, "ping"),
        (1, "tools/list"),
        (2, "server/discover"),
        (3, "nope/x"),
    ] {
        s.send(
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":{"_meta":meta("1900-01-01")}}),
        );
        let e = s.recv_expect();
        assert_eq!(
            e["error"]["code"], -32022,
            "method {method} before discovery"
        );
        assert_eq!(e["id"], id);
        assert_eq!(
            e["error"]["data"]["supported"],
            json!(["2025-11-25", "2026-07-28"])
        );
        assert_eq!(e["error"]["data"]["requested"], "1900-01-01");
    }
    s.send(discover(json!(10), "2026-07-28"));
    s.recv_expect();
    s.send(json!({"jsonrpc":"2.0","id":11,"method":"ping","params":{"_meta":meta("2027-01-01")}}));
    assert_eq!(s.recv_expect()["error"]["code"], -32022);
    s.send(probe_call(json!(12), "n", true));
    s.recv_expect();
    s.send(json!({"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"_meta":meta("2027-01-01"),"name":"connection_probe","arguments":{"nonce":"n"}}}));
    assert_eq!(s.recv_expect()["error"]["code"], -32022);
    // unsupported version on a notification: dropped, no response, server alive
    s.send(json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"_meta":meta("1900-01-01"),"progressToken":1,"progress":1}}));
    s.send(probe_call(json!(14), "alive", true));
    assert_eq!(s.recv_expect()["id"], 14);
}

#[test]
fn unknown_method_with_valid_params_is_method_not_found() {
    let mut s = Server::spawn();
    s.send(json!({"jsonrpc":"2.0","id":1,"method":"nope/method","params":{"_meta":meta("2026-07-28")}}));
    let e = s.recv_expect();
    assert_eq!(e["error"]["code"], -32601);
    assert_eq!(e["id"], 1);

    let mut l = Server::spawn();
    legacy_handshake(&mut l, "2025-11-25");
    l.send(json!({"jsonrpc":"2.0","id":2,"method":"nope/method"}));
    assert_eq!(l.recv_expect()["error"]["code"], -32601);
    l.send(json!({"jsonrpc":"2.0","id":3,"method":"nope/method","params":{}}));
    assert_eq!(l.recv_expect()["error"]["code"], -32601);
}

#[test]
fn modern_request_missing_meta_is_invalid_params() {
    let mut s = Server::spawn();
    s.send(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}));
    let e = s.recv_expect();
    assert_eq!(e["error"]["code"], -32602);
    assert_eq!(e["id"], 1);
    // The server keeps serving afterwards.
    s.send(discover(json!(2), "2026-07-28"));
    assert_eq!(s.recv_expect()["result"]["resultType"], "complete");
}

#[test]
fn unsupported_modern_version_lists_supported_versions() {
    let mut s = Server::spawn();
    s.send(discover(json!(1), "1900-01-01"));
    let e = s.recv_expect();
    assert_eq!(e["error"]["code"], -32022);
    assert_eq!(
        e["error"]["data"]["supported"],
        json!(["2025-11-25", "2026-07-28"])
    );
    assert_eq!(e["error"]["data"]["requested"], "1900-01-01");
    // In-session (after a valid modern request) the same rule applies.
    s.send(discover(json!(2), "2026-07-28"));
    s.recv_expect();
    s.send(
        json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{"_meta":meta("2027-01-01")}}),
    );
    assert_eq!(s.recv_expect()["error"]["code"], -32022);
}
