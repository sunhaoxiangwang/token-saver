//! Supervision tests S1 to S8 (plan 4.5) with production constants, in the gated fixture
//! environment: the library in-process over pipes, the fixture `FileSystem` holding inside a
//! step (step and job markers armed) or at the checkpoint after it (job marker armed only).
//!
//! Detection is observed through the latched cause; the production termination path is observed
//! as the serve loop ending after the latch. The supervisor's forced termination is the backstop.

mod common;

use std::time::{Duration, Instant};

use common::ws::{Call, GatedFs, TempRoot, WsHarness, ws_call};
use common::{cancel, probe_call};
use serde_json::{Value, json};
use token_saver::limits;
use token_saver::supervisor::Cause;

fn slice(id: u64) -> Value {
    ws_call(json!(id), "file_slice", json!({"path": "f.txt"}))
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

/// Detection plus normal termination allowance: TICK plus 300 ms.
const ALLOWANCE: f64 = 0.1 + 0.3;

fn in_window(elapsed: Duration, deadline: f64) -> bool {
    let e = secs(elapsed);
    e >= deadline - 0.05 && e <= deadline + ALLOWANCE
}

fn root_with_file() -> TempRoot {
    let root = TempRoot::new("sup");
    root.write("f.txt", b"hello\nworld\n");
    root
}

#[test]
fn s1_stall_inside_a_step_latches_worker_stall_from_the_step_start() {
    let root = root_with_file();
    let fs = GatedFs::new();
    let hold = fs.hold_inside(Call::Read, None);
    let mut h = WsHarness::start(&root.path, fs);
    h.send(slice(1));
    let ts = hold.wait_engaged(Duration::from_secs(3)).expect("engaged");
    let cause = h.wait_fatal(Duration::from_secs(4));
    let detected = ts.elapsed();
    assert_eq!(cause, Cause::WorkerStall);
    assert!(
        in_window(detected, secs(limits::STEP)),
        "detected at {:.3}s after step start",
        secs(detected)
    );
    assert!(
        h.wait_service_end(Duration::from_secs(1)),
        "serve loop ends after the latch"
    );
    h.assert_silent(Duration::from_millis(200));
    // Backstop: forced termination with the latched code at the shutdown clock.
    let (code, c) = h
        .terminator
        .wait(Duration::from_secs(5))
        .expect("terminator");
    assert_eq!((code, c), (4, Cause::WorkerStall));
    let total = ts.elapsed();
    assert!(
        secs(total) >= 5.0 && secs(total) <= 5.0 + 2.0 * ALLOWANCE,
        "{:.3}",
        secs(total)
    );
    hold.release();
}

#[test]
fn s2_handler_deadline_orphans_the_job_until_it_ends() {
    let root = root_with_file();
    let fs = GatedFs::new();
    let hold = fs.hold_after(Call::Read, None);
    let mut h = WsHarness::start(&root.path, fs);
    h.send(slice(1));
    let t0 = h
        .wait_admitted(1, Duration::from_secs(2))
        .expect("admitted");
    hold.wait_engaged(Duration::from_secs(3)).expect("engaged");
    let r = h.recv(Duration::from_secs(7)).expect("deadline response");
    let at = t0.elapsed();
    assert_eq!(r["error"]["code"], -32603);
    assert_eq!(r["error"]["message"], "processing deadline exceeded");
    assert!(in_window(at, secs(limits::PROCESSING)), "{:.3}", secs(at));
    assert!(h.wait_until(Duration::from_secs(1), |c, p| {
        c.requests == 0 && p.jobs_orphaned == 1 && p.jobs_running == 1 && p.job_slots_available == 7
    }));
    assert_eq!(h.shared.fatal(), Cause::None);
    let release_at = t0 + Duration::from_secs(6);
    std::thread::sleep(release_at.saturating_duration_since(Instant::now()));
    hold.release();
    let released = Instant::now();
    assert!(h.wait_until(Duration::from_millis(300), |c, p| {
        p.jobs_orphaned == 0
            && p.jobs_running == 0
            && p.job_slots_available == 8
            && p.job_descriptors_open == 0
            && c.jobs == 0
    }));
    assert!(released.elapsed() < Duration::from_millis(300));
    h.send(probe_call(json!(2), "after", true));
    assert_eq!(
        h.recv_expect()["result"]["structuredContent"]["nonce"],
        "after"
    );
    h.close_stdin();
    assert!(h.wait_service_end(Duration::from_secs(2)));
    let c = h.registry.counts();
    assert_eq!((c.requests, c.deliveries, c.jobs), (0, 0, 0));
    assert_eq!(h.shared.fatal(), Cause::None);
}

#[test]
fn s3_orphan_past_the_grace_latches_worker_stall_via_the_job_marker() {
    let root = root_with_file();
    let fs = GatedFs::new();
    let hold = fs.hold_after(Call::Read, None);
    let mut h = WsHarness::start(&root.path, fs);
    h.send(slice(1));
    let t0 = h
        .wait_admitted(1, Duration::from_secs(2))
        .expect("admitted");
    hold.wait_engaged(Duration::from_secs(3)).expect("engaged");
    let r = h.recv(Duration::from_secs(7)).expect("deadline response");
    assert_eq!(r["error"]["code"], -32603);
    let cause = h.wait_fatal(Duration::from_secs(4));
    let at = t0.elapsed();
    assert_eq!(cause, Cause::WorkerStall);
    assert!(
        in_window(at, secs(limits::PROCESSING + limits::JOB_GRACE)),
        "{:.3}",
        secs(at)
    );
    hold.release();
}

#[test]
fn s4_eof_with_a_held_job_is_abandoned_with_code_1_at_the_shutdown_budget() {
    let root = root_with_file();
    let fs = GatedFs::new();
    let hold = fs.hold_after(Call::Read, None);
    let mut h = WsHarness::start(&root.path, fs);
    h.send(slice(1));
    let t0 = h
        .wait_admitted(1, Duration::from_secs(2))
        .expect("admitted");
    hold.wait_engaged(Duration::from_secs(3)).expect("engaged");
    std::thread::sleep((t0 + Duration::from_millis(500)).saturating_duration_since(Instant::now()));
    h.close_stdin();
    let (code, cause) = h
        .terminator
        .wait(Duration::from_secs(6))
        .expect("terminator");
    let at = t0.elapsed();
    assert_eq!((code, cause), (1, Cause::None));
    assert!(
        in_window(at, 0.5 + secs(limits::SHUTDOWN)),
        "{:.3}",
        secs(at)
    );
    assert_eq!(h.shared.fatal(), Cause::None);
    hold.release();
}

#[test]
fn s5_stall_during_shutdown_is_latched_before_the_shutdown_deadline() {
    let root = root_with_file();
    let fs = GatedFs::new();
    let hold = fs.hold_inside(Call::Read, None);
    let mut h = WsHarness::start(&root.path, fs);
    h.send(slice(1));
    let ts = hold.wait_engaged(Duration::from_secs(3)).expect("engaged");
    std::thread::sleep((ts + Duration::from_millis(500)).saturating_duration_since(Instant::now()));
    let t_eof = Instant::now();
    h.close_stdin();
    let cause = h.wait_fatal(Duration::from_secs(4));
    let detected = ts.elapsed();
    assert_eq!(cause, Cause::WorkerStall);
    assert!(
        in_window(detected, secs(limits::STEP)),
        "{:.3}",
        secs(detected)
    );
    assert!(h.wait_service_end(Duration::from_secs(1)));
    // The clock started by EOF is not restarted: forced termination at EOF + 3 s with code 4.
    let (code, c) = h
        .terminator
        .wait(Duration::from_secs(5))
        .expect("terminator");
    let at = t_eof.elapsed();
    assert_eq!((code, c), (4, Cause::WorkerStall));
    assert!(in_window(at, secs(limits::SHUTDOWN)), "{:.3}", secs(at));
    hold.release();
}

#[test]
fn s6_queued_work_completes_after_eof_before_the_shutdown_deadline() {
    let root = root_with_file();
    let fs = GatedFs::new();
    fs.set_delay(Duration::from_millis(50));
    let mut h = WsHarness::start(&root.path, fs);
    let t0 = Instant::now();
    for i in 1..=8 {
        h.send(slice(i));
    }
    h.close_stdin();
    let mut ids = Vec::new();
    for _ in 0..8 {
        let r = h.recv(Duration::from_secs(5)).expect("response");
        assert_eq!(r["result"]["isError"], false, "{r}");
        ids.push(r["id"].as_u64().unwrap());
    }
    ids.sort();
    assert_eq!(ids, (1..=8).collect::<Vec<_>>());
    assert!(t0.elapsed() < limits::SHUTDOWN);
    assert!(h.wait_service_end(Duration::from_secs(2)));
    assert!(h.wait_until(Duration::from_secs(1), |c, p| {
        c.requests == 0 && c.deliveries == 0 && c.jobs == 0 && p.job_descriptors_open == 0
    }));
    assert_eq!(h.shared.fatal(), Cause::None);
}

#[test]
fn s7_cancelled_handlers_leave_orphans_that_make_the_ninth_request_busy() {
    let root = root_with_file();
    let fs = GatedFs::new();
    let holds: Vec<_> = (0..8).map(|_| fs.hold_after(Call::Read, None)).collect();
    let mut h = WsHarness::start(&root.path, fs);
    for i in 1..=8 {
        h.send(slice(i));
    }
    // Four workers hold at their checkpoints; the other four jobs wait in the queue. All eight
    // slots are taken either way.
    assert!(h.wait_until(Duration::from_secs(5), |c, p| {
        c.admitted == 8 && p.jobs_running == 4 && p.jobs_queued == 4 && p.job_slots_available == 0
    }));
    assert_eq!(holds.iter().filter(|h| h.engaged_at().is_some()).count(), 4);
    std::thread::sleep(Duration::from_millis(200));
    for i in 1..=8 {
        h.send(cancel(json!(i)));
    }
    assert!(h.wait_until(Duration::from_secs(2), |c, p| {
        c.requests == 0 && p.jobs_orphaned == 8 && p.job_slots_available == 0
    }));
    h.assert_silent(Duration::from_millis(100));
    let t9 = Instant::now();
    h.send(slice(9));
    let r = h.recv(Duration::from_secs(4)).unwrap_or_else(|| {
        panic!(
            "no busy response; counts {:?} pool {:?}",
            h.registry.counts(),
            h.workspace.snapshot()
        )
    });
    let at = t9.elapsed();
    assert_eq!(r["result"]["isError"], true);
    assert_eq!(r["result"]["content"][0]["text"], "busy");
    assert!(in_window(at, secs(limits::SLOT_WAIT)), "{:.3}", secs(at));
    for hold in &holds {
        hold.release();
    }
    assert!(h.wait_until(Duration::from_secs(1), |c, p| {
        p.jobs_orphaned == 0
            && p.job_slots_available == 8
            && p.job_descriptors_open == 0
            && c.jobs == 0
    }));
    h.close_stdin();
    assert!(h.wait_service_end(Duration::from_secs(2)));
    assert_eq!(h.shared.fatal(), Cause::None);
}

#[test]
fn s8_job_descriptor_count_returns_to_zero_after_mixed_work() {
    let root = root_with_file();
    root.mkdir("d/e");
    root.write("d/e/g.txt", b"needle\n");
    let mut h = WsHarness::start(&root.path, GatedFs::new());
    for i in 1..=6 {
        h.send(ws_call(json!(i), "workspace_outline", json!({"depth": 3})));
        h.send(ws_call(
            json!(100 + i),
            "text_search",
            json!({"query": "needle"}),
        ));
        h.send(slice(200 + i));
        // At most eight requests in flight: the request bound is nine.
        for _ in 0..3 {
            let r = h.recv_expect();
            assert_eq!(r["result"]["isError"], false, "{r}");
        }
    }
    assert!(h.wait_until(Duration::from_secs(1), |c, p| {
        c.jobs == 0 && p.job_descriptors_open == 0 && p.jobs_running == 0 && p.jobs_queued == 0
    }));
    assert_eq!(h.workspace.snapshot().job_slots_available, 8);
}
