//! In-process lifecycle tests: admission, queued cancellation, commitment race, deadlines,
//! delivery stalls, floods, duplicates, and supervision, using the production transport,
//! registry, and supervisor with a controllable service.

mod common;

use std::time::{Duration, Instant};

use common::fixture::{Harness, HoldMode};
use common::*;
use serde_json::json;
use token_saver::limits;
use token_saver::supervisor::{Cause, Participant};

fn hold(id: u64) -> serde_json::Value {
    tools_call(json!(id), json!({"nonce": format!("hold{id}")}), true)
}

#[test]
fn eight_admitted_one_queued_then_overflow() {
    let mut h = Harness::start(HoldMode::Cooperative);
    for i in 1..=8 {
        h.send(hold(i));
    }
    assert!(h.wait_admitted(8, Duration::from_secs(2)));
    h.send(hold(9));
    assert!(h.wait_requests(9, Duration::from_secs(2)));
    let c = h.registry.counts();
    assert_eq!(c.admitted, 8);
    assert_eq!(c.available_permits, 0);
    h.assert_silent(Duration::from_millis(200));
    h.send(hold(10));
    assert_eq!(
        h.wait_fatal(Duration::from_secs(2)),
        Cause::AdmissionOverflow
    );
    let (code, cause) = h
        .terminator
        .wait(Duration::from_secs(5))
        .expect("terminator fires");
    assert_eq!((code, cause), (3, Cause::AdmissionOverflow));
}

#[test]
fn two_request_burst_with_free_permits_is_admitted_not_overflow() {
    let mut h = Harness::start(HoldMode::Cooperative);
    let burst = format!("{}\n{}\n", hold(1), hold(2));
    h.send_raw(burst.as_bytes());
    assert!(h.wait_admitted(2, Duration::from_secs(2)));
    assert_eq!(h.shared.fatal(), Cause::None);
    h.gate.release("hold1");
    h.gate.release("hold2");
    let a = h.recv_expect();
    let b = h.recv_expect();
    let mut ids = vec![a["id"].as_u64().unwrap(), b["id"].as_u64().unwrap()];
    ids.sort();
    assert_eq!(ids, vec![1, 2]);
    assert!(h.wait_requests(0, Duration::from_secs(2)));
}

#[test]
fn queued_request_cancelled_before_dispatch_is_skipped_without_response() {
    let mut h = Harness::start(HoldMode::Cooperative);
    for i in 1..=8 {
        h.send(hold(i));
    }
    assert!(h.wait_admitted(8, Duration::from_secs(2)));
    h.send(hold(9));
    assert!(h.wait_requests(9, Duration::from_secs(2)));
    h.send(cancel(json!(9)));
    std::thread::sleep(Duration::from_millis(100));
    h.gate.release_all();
    let mut ids = Vec::new();
    for _ in 0..8 {
        ids.push(h.recv_expect()["id"].as_u64().unwrap());
    }
    ids.sort();
    assert_eq!(ids, (1..=8).collect::<Vec<_>>());
    h.assert_silent(Duration::from_millis(500));
    assert!(h.wait_requests(0, Duration::from_secs(2)));
    let c = h.registry.counts();
    assert_eq!(c.available_permits, limits::MAX_ADMITTED);
    assert_eq!(
        h.service.calls.load(std::sync::atomic::Ordering::SeqCst),
        8,
        "the cancelled request never ran"
    );
    assert_eq!(h.shared.fatal(), Cause::None);
}

#[test]
fn cancelling_an_admitted_request_suppresses_its_output_and_frees_capacity() {
    let mut h = Harness::start(HoldMode::Cooperative);
    h.send(hold(1));
    assert!(h.wait_admitted(1, Duration::from_secs(2)));
    h.send(cancel(json!(1)));
    assert!(
        h.wait_requests(0, Duration::from_secs(2)),
        "handler returned and output was suppressed"
    );
    h.assert_silent(Duration::from_millis(300));
    assert_eq!(h.registry.counts().available_permits, limits::MAX_ADMITTED);
    // The server keeps working.
    h.send(probe_call(json!(2), "after", true));
    assert_eq!(
        h.recv_expect()["result"]["structuredContent"]["nonce"],
        "after"
    );
}

#[test]
fn cancel_after_commitment_is_late_and_response_is_still_delivered() {
    let mut h = Harness::start(HoldMode::Cooperative);
    h.write_gate.block_writes();
    h.send(probe_call(json!(1), "fast", true));
    assert!(
        h.write_gate.wait_began(Duration::from_secs(2)),
        "write began while blocked"
    );
    h.send(cancel(json!(1)));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(h.registry.counts().deliveries, 1);
    h.write_gate.release();
    let r = h.recv_expect();
    assert_eq!(r["id"], 1);
    assert_eq!(r["result"]["structuredContent"]["nonce"], "fast");
    assert!(h.wait_requests(0, Duration::from_secs(2)));
    assert_eq!(h.shared.fatal(), Cause::None);
}

#[test]
fn duplicate_of_a_queued_id_is_fatal_for_valid_and_malformed_frames() {
    for malformed in [false, true] {
        let mut h = Harness::start(HoldMode::Cooperative);
        for i in 1..=8 {
            h.send(hold(i));
        }
        assert!(h.wait_admitted(8, Duration::from_secs(2)));
        h.send(hold(9));
        assert!(h.wait_requests(9, Duration::from_secs(2)));
        if malformed {
            h.send_line(r#"{"jsonrpc":"2.0","id":9}"#);
        } else {
            h.send(json!({"jsonrpc":"2.0","id":9,"method":"ping","params":{"_meta":meta("2026-07-28")}}));
        }
        assert_eq!(
            h.wait_fatal(Duration::from_secs(2)),
            Cause::DuplicateId,
            "malformed={malformed}"
        );
        assert_eq!(
            h.terminator.wait(Duration::from_secs(5)).map(|d| d.0),
            Some(3)
        );
    }
}

#[test]
fn duplicate_of_an_admitted_id_and_of_a_pending_error_id_are_fatal() {
    let mut h = Harness::start(HoldMode::Cooperative);
    h.send(hold(1));
    assert!(h.wait_admitted(1, Duration::from_secs(2)));
    h.send(json!({"jsonrpc":"2.0","id":1,"method":"ping","params":{"_meta":meta("2026-07-28")}}));
    assert_eq!(h.wait_fatal(Duration::from_secs(2)), Cause::DuplicateId);

    let mut h2 = Harness::start(HoldMode::Cooperative);
    h2.write_gate.block_writes();
    h2.send_line(r#"{"jsonrpc":"2.0","id":"e1","method":5}"#);
    assert!(h2.write_gate.wait_began(Duration::from_secs(2)));
    h2.send(
        json!({"jsonrpc":"2.0","id":"e1","method":"ping","params":{"_meta":meta("2026-07-28")}}),
    );
    assert_eq!(h2.wait_fatal(Duration::from_secs(2)), Cause::DuplicateId);
}

#[test]
fn id_reuse_after_retirement_is_accepted() {
    let mut h = Harness::start(HoldMode::Cooperative);
    h.send(probe_call(json!(1), "one", true));
    assert_eq!(
        h.recv_expect()["result"]["structuredContent"]["nonce"],
        "one"
    );
    assert!(h.wait_requests(0, Duration::from_secs(2)));
    h.send(probe_call(json!(1), "two", true));
    assert_eq!(
        h.recv_expect()["result"]["structuredContent"]["nonce"],
        "two"
    );
    assert_eq!(h.shared.fatal(), Cause::None);
}

#[test]
fn cooperative_hold_returns_processing_deadline_error_at_five_seconds() {
    let mut h = Harness::start(HoldMode::Cooperative);
    let t0 = Instant::now();
    h.send(hold(1));
    let r = h.recv(Duration::from_secs(7)).expect("deadline error");
    let elapsed = t0.elapsed();
    assert_eq!(r["error"]["code"], -32603);
    assert_eq!(r["error"]["message"], "processing deadline exceeded");
    assert!(
        elapsed >= Duration::from_secs(5) && elapsed < Duration::from_millis(5800),
        "{elapsed:?}"
    );
    assert!(h.wait_requests(0, Duration::from_secs(2)));
    assert_eq!(h.shared.fatal(), Cause::None);
}

#[test]
fn resistant_hold_is_forced_by_the_admission_watchdog() {
    let mut h = Harness::start(HoldMode::Resistant);
    let t0 = Instant::now();
    h.send(hold(1));
    h.assert_silent(Duration::from_secs(6));
    let cause = h.wait_fatal(Duration::from_secs(6));
    let at = t0.elapsed();
    assert_eq!(cause, Cause::AdmissionDeadline);
    assert!(
        at >= Duration::from_secs(10) && at < Duration::from_millis(10600),
        "{at:?}"
    );
    let (code, cause) = h
        .terminator
        .wait(Duration::from_secs(5))
        .expect("forced exit");
    assert_eq!((code, cause), (4, Cause::AdmissionDeadline));
    assert!(
        t0.elapsed() < Duration::from_millis(13700),
        "{:?}",
        t0.elapsed()
    );
}

#[test]
fn blocked_runtime_thread_is_still_terminated_by_the_external_supervisor() {
    let mut h = Harness::start(HoldMode::BlockThread(14));
    let t0 = Instant::now();
    h.send(hold(1));
    let (code, cause) = h
        .terminator
        .wait(Duration::from_secs(15))
        .expect("terminated despite blocked runtime");
    assert_eq!((code, cause), (4, Cause::AdmissionDeadline));
    assert!(
        t0.elapsed() < Duration::from_millis(13700),
        "{:?}",
        t0.elapsed()
    );
}

#[test]
fn blocked_write_fires_delivery_deadline_with_free_slots() {
    let mut h = Harness::start(HoldMode::Cooperative);
    h.write_gate.block_writes();
    let t0 = Instant::now();
    h.send(probe_call(json!(1), "stall", true));
    assert!(h.write_gate.wait_began(Duration::from_secs(2)));
    let c = h.registry.counts();
    assert_eq!(c.deliveries, 1);
    assert_eq!(c.available_error_budget, limits::ERROR_BUDGET);
    let cause = h.wait_fatal(Duration::from_secs(7));
    let at = t0.elapsed();
    assert_eq!(cause, Cause::DeliveryDeadline);
    assert!(
        at >= Duration::from_secs(5) && at < Duration::from_millis(5600),
        "{at:?}"
    );
    assert_eq!(
        h.terminator.wait(Duration::from_secs(5)),
        Some((4, Cause::DeliveryDeadline))
    );
}

#[test]
fn blocked_flush_fires_delivery_deadline() {
    let mut h = Harness::start(HoldMode::Cooperative);
    h.write_gate.block_flushes();
    h.send(probe_call(json!(1), "stall", true));
    assert!(h.write_gate.wait_began(Duration::from_secs(2)));
    assert!(h.write_gate.flush_began());
    assert_eq!(
        h.wait_fatal(Duration::from_secs(7)),
        Cause::DeliveryDeadline
    );
    assert_eq!(
        h.terminator.wait(Duration::from_secs(5)).map(|d| d.0),
        Some(4)
    );
}

#[test]
fn single_malformed_frame_into_blocked_output_times_out_with_free_slots() {
    let mut h = Harness::start(HoldMode::Cooperative);
    h.write_gate.block_writes();
    let t0 = Instant::now();
    h.send_line("{not json");
    assert!(h.write_gate.wait_began(Duration::from_secs(2)));
    let c = h.registry.counts();
    assert_eq!(c.deliveries, 1);
    assert_eq!(c.available_error_budget, limits::ERROR_BUDGET - 1);
    assert_eq!(c.available_permits, limits::MAX_ADMITTED);
    assert_eq!(
        h.wait_fatal(Duration::from_secs(7)),
        Cause::DeliveryDeadline
    );
    assert!(t0.elapsed() < Duration::from_millis(5600));
    assert_eq!(
        h.terminator.wait(Duration::from_secs(5)).map(|d| d.0),
        Some(4)
    );
}

#[test]
fn error_flood_exhausts_budget_and_terminates_with_code_3() {
    let mut h = Harness::start(HoldMode::Cooperative);
    h.write_gate.block_writes();
    for _ in 0..5 {
        h.send_line("{not json");
    }
    assert_eq!(h.wait_fatal(Duration::from_secs(2)), Cause::ErrorFlood);
    assert_eq!(
        h.terminator.wait(Duration::from_secs(5)).map(|d| d.0),
        Some(3)
    );
}

#[test]
fn unsupported_notification_flood_is_dropped_and_bounded() {
    let mut h = Harness::start(HoldMode::Cooperative);
    let mut burst = String::new();
    for i in 0..10_000 {
        burst.push_str(&json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":i,"progress":1}}).to_string());
        burst.push('\n');
    }
    h.send_raw(burst.as_bytes());
    h.send(probe_call(json!(1), "alive", true));
    let r = h
        .recv(Duration::from_secs(10))
        .expect("server still answers");
    assert_eq!(r["result"]["structuredContent"]["nonce"], "alive");
    let c = h.registry.counts();
    assert_eq!(c.initialized_outstanding, 0);
    assert_eq!(
        h.service
            .initialized_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert_eq!(h.shared.fatal(), Cause::None);
}

#[test]
fn initialized_forwarding_is_credited_once_per_initialize_and_acknowledged() {
    let mut h = Harness::start(HoldMode::Cooperative);
    for round in 1..=3u64 {
        h.send(initialize(json!(round), "2025-11-25"));
        assert_eq!(h.recv_expect()["result"]["protocolVersion"], "2025-11-25");
        h.send(initialized());
        h.send(initialized());
        let deadline = Instant::now() + Duration::from_secs(2);
        while h
            .service
            .initialized_calls
            .load(std::sync::atomic::Ordering::SeqCst)
            < round as usize
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            h.service
                .initialized_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            round as usize
        );
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(h.registry.counts().initialized_outstanding, 0);
    }
}

#[test]
fn registry_transaction_marker_stall_is_detected_without_a_lock() {
    let h = Harness::start(HoldMode::Cooperative);
    // Simulate a participant that armed its marker and then stalled before publication.
    h.shared.arm_txn(Participant::Reader, limits::TRANSACTION);
    let (code, cause) = h
        .terminator
        .wait(Duration::from_secs(6))
        .expect("stall detected");
    assert_eq!((code, cause), (4, Cause::RegistryStall));
}

#[test]
fn queued_request_without_dispatch_fires_acceptance_deadline() {
    let mut h = Harness::start_without_service();
    let t0 = Instant::now();
    h.send(probe_call(json!(1), "queued", true));
    assert!(h.wait_requests(1, Duration::from_secs(2)));
    assert_eq!(h.registry.counts().admitted, 0);
    let cause = h.wait_fatal(Duration::from_secs(12));
    let at = t0.elapsed();
    assert_eq!(cause, Cause::AcceptanceDeadline);
    assert!(
        at >= Duration::from_secs(10) && at < Duration::from_millis(10600),
        "{at:?}"
    );
    assert_eq!(
        h.terminator.wait(Duration::from_secs(5)),
        Some((4, Cause::AcceptanceDeadline))
    );
}

#[test]
fn idle_harness_never_terminates() {
    let h = Harness::start(HoldMode::Cooperative);
    assert!(h.terminator.wait(Duration::from_secs(11)).is_none());
    assert_eq!(h.shared.fatal(), Cause::None);
}

#[test]
fn eof_then_write_failure_records_the_fatal_cause() {
    let mut h = Harness::start(HoldMode::Cooperative);
    h.write_gate.block_writes();
    h.send(probe_call(json!(1), "x", true));
    assert!(h.write_gate.wait_began(Duration::from_secs(2)));
    h.close_stdin();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(h.shared.orderly(), token_saver::diag::Trigger::Eof);
    assert_eq!(h.shared.fatal(), Cause::None);
    h.write_gate.fail_and_release();
    assert_eq!(
        h.wait_fatal(Duration::from_secs(2)),
        Cause::WriteError,
        "fatal cause recorded after EOF"
    );
    assert_eq!(h.shared.exit_code(false), 4);
    assert_eq!(
        h.terminator.wait(Duration::from_secs(5)).map(|d| d.0),
        Some(4)
    );
}

#[test]
fn eof_with_blocked_writer_is_forced_with_code_1_at_shutdown_budget() {
    let mut h = Harness::start(HoldMode::Cooperative);
    h.write_gate.block_writes();
    h.send(probe_call(json!(1), "x", true));
    assert!(h.write_gate.wait_began(Duration::from_secs(2)));
    let t0 = Instant::now();
    h.close_stdin();
    let (code, cause) = h
        .terminator
        .wait(Duration::from_secs(5))
        .expect("forced orderly shutdown");
    let at = t0.elapsed();
    assert_eq!((code, cause), (1, Cause::None));
    assert!(
        at >= Duration::from_secs(3) && at < Duration::from_millis(3600),
        "{at:?}"
    );
}

#[test]
fn fatal_before_eof_keeps_fatal_code() {
    let mut h = Harness::start(HoldMode::Cooperative);
    h.send_line("{not json");
    h.send_line(r#"{"jsonrpc":"2.0","id":"dup","method":"ping","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#);
    h.recv_expect();
    h.recv_expect();
    h.send(json!({"jsonrpc":"2.0","id":"x","method":"ping","params":{"_meta":meta("2026-07-28")}}));
    h.recv_expect();
    // Hold one admitted request, then duplicate its id (fatal), then EOF.
    h.send(hold(7));
    assert!(h.wait_admitted(1, Duration::from_secs(2)));
    h.send(json!({"jsonrpc":"2.0","id":7,"method":"ping","params":{"_meta":meta("2026-07-28")}}));
    assert_eq!(h.wait_fatal(Duration::from_secs(2)), Cause::DuplicateId);
    h.close_stdin();
    assert_eq!(h.shared.exit_code(false), 3);
    assert_eq!(
        h.terminator.wait(Duration::from_secs(5)).map(|d| d.0),
        Some(3)
    );
}

/// Measurement, not an acceptance threshold on the SDK: sustained completion and re-admission
/// cycles, then a drain. Reports alive runtime tasks and registry counters before, during, and
/// after load. Run explicitly: `cargo test --test fixture_lifecycle -- --ignored bookkeeping`.
#[test]
#[ignore]
fn sdk_bookkeeping_under_sustained_load_and_after_drain() {
    let mut h = Harness::start(HoldMode::Cooperative);
    let handle = h.runtime.clone().expect("runtime handle");
    let alive = |handle: &tokio::runtime::Handle| handle.metrics().num_alive_tasks();
    // Baseline: idle after one completed request, so the SDK's long-lived service task is counted.
    h.send(probe_call(json!(0), "warm", true));
    h.recv_expect();
    std::thread::sleep(Duration::from_millis(200));
    let baseline = alive(&handle);
    let t0 = Instant::now();
    let mut sent = 0u64;
    let mut received = 0u64;
    let mut peak_alive = baseline;
    let mut samples = Vec::new();
    while t0.elapsed() < Duration::from_secs(20) {
        for _ in 0..8 {
            sent += 1;
            h.send(probe_call(json!(sent), "load", true));
        }
        for _ in 0..8 {
            if h.recv(Duration::from_secs(5)).is_some() {
                received += 1;
            }
        }
        let a = alive(&handle);
        peak_alive = peak_alive.max(a);
        if sent.is_multiple_of(8000) {
            samples.push((t0.elapsed().as_secs(), a, h.registry.counts().requests));
        }
    }
    assert_eq!(sent, received);
    let during = alive(&handle);
    let drain_start = Instant::now();
    let mut after = during;
    while drain_start.elapsed() < Duration::from_secs(5) {
        after = alive(&handle);
        if after <= baseline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let drained_in = drain_start.elapsed();
    eprintln!(
        "MEASURED bookkeeping: requests={sent} baseline_alive_tasks={baseline} peak_alive_tasks={peak_alive} alive_at_end_of_load={during} alive_after_drain={after} drained_in={drained_in:?} samples={samples:?} counts={:?}",
        h.registry.counts()
    );
    assert_eq!(
        h.registry.counts().requests,
        0,
        "application accounting drained"
    );
    assert!(
        after <= baseline,
        "alive tasks returned to baseline within 5 s: after={after} baseline={baseline}"
    );
}

#[test]
fn eof_with_held_admitted_work_is_forced_with_code_1() {
    let mut h = Harness::start(HoldMode::Cooperative);
    h.send(hold(1));
    assert!(h.wait_admitted(1, Duration::from_secs(2)));
    let t0 = Instant::now();
    h.close_stdin();
    let (code, cause) = h.terminator.wait(Duration::from_secs(5)).expect("forced");
    assert_eq!((code, cause), (1, Cause::None));
    assert!(t0.elapsed() >= Duration::from_secs(3) && t0.elapsed() < Duration::from_millis(3600));
}

#[test]
fn eof_with_queued_work_dispatches_and_delivers_it_before_exit() {
    let mut h = Harness::start(HoldMode::Cooperative);
    for i in 1..=8 {
        h.send(hold(i));
    }
    assert!(h.wait_admitted(8, Duration::from_secs(2)));
    h.send(probe_call(json!(9), "queued", true));
    assert!(h.wait_requests(9, Duration::from_secs(2)));
    h.close_stdin();
    h.gate.release_all();
    let mut ids = Vec::new();
    for _ in 0..9 {
        ids.push(h.recv_expect()["id"].as_u64().unwrap());
    }
    ids.sort();
    assert_eq!(ids, (1..=9).collect::<Vec<_>>());
    assert!(h.wait_service_end(Duration::from_secs(6)));
    assert_eq!(h.registry.counts().requests, 0);
    assert_eq!(h.registry.counts().deliveries, 0);
    assert_eq!(h.shared.exit_code(false), 0);
}
