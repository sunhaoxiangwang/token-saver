//! F03 (TS-M1-CODE-REVIEW-001): root acquisition runs under the armed startup marker, so a stalled
//! filesystem lookup latches `startup_stall` (exit 4) with the production supervisor.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::fixture::RecordingTerminator;
use common::ws::TempRoot;
use token_saver::diag::Diag;
use token_saver::limits;
use token_saver::supervisor::{self, Cause, Participant, Shared};
use token_saver::workspace::confine::{acquire_root, open_root};
use tokio_util::sync::CancellationToken;

#[test]
fn a_held_root_acquisition_latches_startup_stall_and_is_terminated() {
    let root = TempRoot::new("startup");
    let shared = Arc::new(Shared::new(CancellationToken::new(), Diag::disabled()));
    let terminator = Arc::new(RecordingTerminator::default());
    let armed_at = Instant::now();
    shared.arm_txn(Participant::Startup, limits::STARTUP);
    supervisor::spawn(shared.clone(), terminator.clone(), limits::TICK);
    let path = root.path.to_str().unwrap().to_owned();
    // The opener stalls for 4 s, past the 3 s startup budget, then succeeds.
    let fd = acquire_root(&shared, || {
        std::thread::sleep(Duration::from_secs(4));
        open_root(&path)
    })
    .expect("the stalled open eventually succeeds");
    drop(fd);
    assert_eq!(
        shared.fatal(),
        Cause::StartupStall,
        "detected while the open was held"
    );
    let (code, cause) = terminator.wait(Duration::from_secs(5)).expect("terminator");
    let at = armed_at.elapsed().as_secs_f64();
    assert_eq!((code, cause), (4, Cause::StartupStall));
    // Detection at 3 s, forced termination at the shutdown clock 3 s later.
    assert!((6.0..=6.5).contains(&at), "{at:.3}");
}

#[test]
fn a_fast_root_acquisition_under_the_marker_succeeds_and_an_unarmed_one_is_refused() {
    let root = TempRoot::new("startup-ok");
    let shared = Arc::new(Shared::new(CancellationToken::new(), Diag::disabled()));
    let path = root.path.to_str().unwrap().to_owned();
    assert!(
        acquire_root(&shared, || open_root(&path)).is_err(),
        "marker not armed"
    );
    shared.arm_txn(Participant::Startup, limits::STARTUP);
    let fd = acquire_root(&shared, || open_root(&path)).expect("opened");
    drop(fd);
    shared.clear_txn(Participant::Startup);
    assert_eq!(shared.fatal(), Cause::None);
}
