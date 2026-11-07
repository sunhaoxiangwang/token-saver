//! Legacy (initialize handshake) wire tests against the production binary. SDK-independent.

mod common;

use std::time::Duration;

use common::*;
use serde_json::json;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn is_instance_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[test]
fn legacy_2025_11_25_handshake_list_call_and_ping() {
    let mut s = Server::spawn();
    let init = legacy_handshake(&mut s, "2025-11-25");
    let r = &init["result"];
    assert_eq!(r["protocolVersion"], "2025-11-25");
    assert!(r["capabilities"]["tools"].is_object());
    assert_eq!(r["serverInfo"]["name"], "token-saver");
    assert_eq!(r["serverInfo"]["version"], VERSION);
    assert!(
        r.get("resultType").is_none(),
        "legacy results omit resultType"
    );
    assert!(r["instructions"].as_str().unwrap().len() <= 512);

    s.send(tools_list(json!(2), false));
    let list = s.recv_expect();
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    let t = &tools[0];
    assert_eq!(t["name"], "connection_probe");
    assert_eq!(t["annotations"]["readOnlyHint"], true);
    assert_eq!(t["annotations"]["destructiveHint"], false);
    assert_eq!(t["annotations"]["idempotentHint"], true);
    assert_eq!(t["annotations"]["openWorldHint"], false);
    assert_eq!(t["inputSchema"]["additionalProperties"], false);
    assert_eq!(t["outputSchema"]["additionalProperties"], false);
    assert!(list["result"].get("resultType").is_none());
    assert!(list["result"].get("ttlMs").is_none());

    s.send(probe_call(json!(3), "abc123", false));
    let call = s.recv_expect();
    let r = &call["result"];
    assert_eq!(r["isError"], false);
    let sc = &r["structuredContent"];
    assert_eq!(sc["nonce"], "abc123");
    assert_eq!(sc["status"], "ok");
    assert_eq!(sc["server_version"], VERSION);
    assert!(is_instance_id(sc["instance_id"].as_str().unwrap()));
    assert_eq!(
        sc.as_object().unwrap().len(),
        4,
        "exactly four output fields"
    );
    let mirror: serde_json::Value =
        serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(&mirror, sc, "text mirror equals structured content");
    assert_eq!(r["content"][0]["text"].as_str().unwrap(), compact(sc));

    s.send(json!({"jsonrpc":"2.0","id":4,"method":"ping"}));
    assert_eq!(s.recv_expect()["result"], json!({}));

    s.close_stdin();
    let status = s.wait_exit(Duration::from_secs(2)).expect("exit after EOF");
    assert_eq!(exit_code(status), 0);
}

#[test]
fn older_legacy_versions_fall_back_to_2025_11_25() {
    for version in ["2025-06-18", "2025-03-26", "2024-11-05"] {
        let mut s = Server::spawn();
        s.send(initialize(json!(1), version));
        let resp = s.recv_expect();
        assert_eq!(
            resp["result"]["protocolVersion"], "2025-11-25",
            "requested {version}"
        );
        s.send(initialized());
        s.send(probe_call(json!(2), "n1", false));
        assert_eq!(
            s.recv_expect()["result"]["structuredContent"]["status"],
            "ok"
        );
    }
}

#[test]
fn repeated_initialize_is_answered_and_service_continues() {
    let mut s = Server::spawn();
    legacy_handshake(&mut s, "2025-11-25");
    s.send(initialize(json!(9), "2025-11-25"));
    let again = s.recv_expect();
    assert_eq!(again["id"], 9);
    assert_eq!(again["result"]["protocolVersion"], "2025-11-25");
    s.send(initialized());
    s.send(probe_call(json!(10), "again", false));
    assert_eq!(
        s.recv_expect()["result"]["structuredContent"]["nonce"],
        "again"
    );
}

#[test]
fn probe_result_stays_within_1024_bytes_for_extreme_nonces() {
    let mut s = Server::spawn();
    legacy_handshake(&mut s, "2025-11-25");
    for nonce in ["A", &"Z".repeat(64)] {
        s.send(probe_call(json!(nonce.len()), nonce, false));
        let line = s.recv_raw(Duration::from_secs(5)).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&line).unwrap();
        let result_bytes = compact(&v["result"]).len();
        assert!(result_bytes <= 1024, "result is {result_bytes} bytes");
        assert!(
            line.len() <= 29 + 256 + 1024 + 1,
            "frame is {} bytes",
            line.len()
        );
        assert_eq!(v["result"]["structuredContent"]["nonce"], nonce);
    }
}

#[test]
fn instance_id_is_stable_within_a_process_and_distinct_across_processes() {
    let mut a = Server::spawn();
    legacy_handshake(&mut a, "2025-11-25");
    a.send(probe_call(json!(1), "x1", false));
    let id1 = a.recv_expect()["result"]["structuredContent"]["instance_id"]
        .as_str()
        .unwrap()
        .to_owned();
    a.send(probe_call(json!(2), "x2", false));
    let id2 = a.recv_expect()["result"]["structuredContent"]["instance_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(id1, id2);
    let startup = a.startup_instance_id().expect("startup line");
    assert_eq!(startup, id1, "stderr startup id matches tool result");

    let mut b = Server::spawn();
    legacy_handshake(&mut b, "2025-11-25");
    b.send(probe_call(json!(1), "x1", false));
    let id3 = b.recv_expect()["result"]["structuredContent"]["instance_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(id1, id3);
}

#[test]
fn initialized_notification_is_forwarded_once_per_initialize_and_duplicates_dropped() {
    let mut s = Server::spawn();
    legacy_handshake(&mut s, "2025-11-25");
    s.send(initialized());
    s.send(initialized());
    s.send(json!({"jsonrpc":"2.0","id":2,"method":"ping"}));
    assert_eq!(s.recv_expect()["id"], 2);
    s.close_stdin();
    assert_eq!(exit_code(s.wait_exit(Duration::from_secs(2)).unwrap()), 0);
    let stderr = s.stderr_after_exit();
    // Duplicates are dropped either for lack of credit or because the forwarded notification is
    // still outstanding; both are the bounded outcome. Exactly two drops, so exactly one forward.
    let drops = stderr
        .iter()
        .filter(|l| {
            l.contains("notification.dropped reason=no_credit")
                || l.contains("notification.dropped reason=outstanding")
        })
        .count();
    assert_eq!(
        drops, 2,
        "two duplicates dropped, first forwarded: {stderr:#?}"
    );
}
