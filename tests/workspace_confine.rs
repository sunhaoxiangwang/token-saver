//! Authority contract tests C1 to C12 (plan 3.5): symlinks, replacement races, root renames,
//! move-out behaviour, FIFOs, sockets, and unknown entry types, through the in-process harness.

mod common;

use std::time::{Duration, Instant};

use common::ws::{Call, GatedFs, TempRoot, WsHarness, ws_call};
use serde_json::{Value, json};
use token_saver::workspace::confine::open_root;

fn text(v: &Value) -> String {
    common::ws::content_of(v["result"]["content"][0]["text"].as_str().unwrap())
}

fn meta(v: &Value) -> Value {
    common::ws::meta_of(v["result"]["content"][0]["text"].as_str().unwrap())
}

fn is_error(v: &Value, message: &str) -> bool {
    v["result"]["isError"] == true && text(v) == message
}

fn slice_call(id: u64, path: &str) -> Value {
    ws_call(json!(id), "file_slice", json!({"path": path}))
}

#[test]
fn c1_pre_existing_symlinks_are_invalid_paths() {
    let root = TempRoot::new("c1");
    let outside = root.outside();
    std::fs::create_dir_all(outside.join("dir")).unwrap();
    std::fs::write(outside.join("dir/file"), b"OUTSIDE").unwrap();
    std::fs::write(outside.join("secret"), b"OUTSIDE").unwrap();
    root.symlink(outside.join("dir").to_str().unwrap(), "link");
    root.symlink(outside.join("secret").to_str().unwrap(), "linkfile");
    root.write("real/file", b"inside\n");
    let mut h = WsHarness::start(&root.path, GatedFs::new());
    h.send(slice_call(1, "link/file"));
    assert!(is_error(&h.recv_expect(), "invalid path"));
    h.send(slice_call(2, "linkfile"));
    assert!(is_error(&h.recv_expect(), "invalid path"));
    h.send(slice_call(3, "real/file"));
    assert_eq!(text(&h.recv_expect()), "1\tinside");
    h.send(ws_call(json!(4), "workspace_outline", json!({})));
    let out = text(&h.recv_expect());
    assert!(out.contains("l link\n") && out.contains("l linkfile"));
    assert!(!out.contains("OUTSIDE"));
    // A symlinked root is refused at open.
    let link_root = root.outside().join("rootlink");
    std::os::unix::fs::symlink(&root.path, &link_root).unwrap();
    assert!(open_root(link_root.to_str().unwrap()).is_err());
}

#[test]
fn c2_component_replaced_by_symlink_after_ancestor_open_is_invalid() {
    let root = TempRoot::new("c2");
    let outside = root.outside();
    std::fs::create_dir_all(outside.join("b2")).unwrap();
    std::fs::write(outside.join("b2/file"), b"OUTSIDE").unwrap();
    root.write("a/b/file", b"inside\n");
    let fs = GatedFs::new();
    let hold = fs.hold_after(Call::OpenDir, Some(b"a"));
    let mut h = WsHarness::start(&root.path, fs);
    h.send(slice_call(1, "a/b/file"));
    hold.wait_engaged(Duration::from_secs(3)).expect("engaged");
    std::fs::remove_dir_all(root.path.join("a/b")).unwrap();
    std::os::unix::fs::symlink(outside.join("b2"), root.path.join("a/b")).unwrap();
    hold.release();
    let r = h.recv_expect();
    assert!(is_error(&r, "invalid path"), "{r}");
}

#[test]
fn c3_ancestor_replaced_by_symlink_after_open_reads_the_original() {
    let root = TempRoot::new("c3");
    let outside = root.outside();
    std::fs::create_dir_all(outside.join("x/b")).unwrap();
    std::fs::write(outside.join("x/b/file"), b"OUTSIDE").unwrap();
    root.write("a/b/file", b"inside\n");
    let fs = GatedFs::new();
    let hold = fs.hold_after(Call::OpenDir, Some(b"b"));
    let mut h = WsHarness::start(&root.path, fs);
    h.send(slice_call(1, "a/b/file"));
    hold.wait_engaged(Duration::from_secs(3)).expect("engaged");
    std::fs::rename(root.path.join("a"), root.path.join("a_moved")).unwrap();
    std::os::unix::fs::symlink(outside.join("x"), root.path.join("a")).unwrap();
    hold.release();
    let r = h.recv_expect();
    assert_eq!(text(&r), "1\tinside", "{r}");
}

#[test]
fn c4_final_file_swapped_for_symlink_after_directory_open_is_invalid() {
    let root = TempRoot::new("c4");
    let outside = root.outside();
    std::fs::write(outside.join("file"), b"OUTSIDE").unwrap();
    root.write("a/file", b"inside\n");
    let fs = GatedFs::new();
    let hold = fs.hold_after(Call::OpenDir, Some(b"a"));
    let mut h = WsHarness::start(&root.path, fs);
    h.send(slice_call(1, "a/file"));
    hold.wait_engaged(Duration::from_secs(3)).expect("engaged");
    std::fs::remove_file(root.path.join("a/file")).unwrap();
    std::os::unix::fs::symlink(outside.join("file"), root.path.join("a/file")).unwrap();
    hold.release();
    let r = h.recv_expect();
    assert!(is_error(&r, "invalid path"), "{r}");
}

#[test]
fn c5_renamed_root_is_followed_and_the_old_path_is_unreachable() {
    let root = TempRoot::new("c5");
    root.write("file", b"original\n");
    let mut h = WsHarness::start(&root.path, GatedFs::new());
    let moved = root.outside().join("moved-root");
    std::fs::rename(&root.path, &moved).unwrap();
    std::fs::create_dir_all(&root.path).unwrap();
    std::fs::write(root.path.join("sentinel.txt"), b"SENTINEL").unwrap();
    h.send(slice_call(1, "file"));
    assert_eq!(text(&h.recv_expect()), "1\toriginal");
    h.send(slice_call(2, "sentinel.txt"));
    assert!(is_error(&h.recv_expect(), "not found"));
    h.send(ws_call(json!(3), "workspace_outline", json!({})));
    let out = text(&h.recv_expect());
    assert!(out.contains("f file") && !out.contains("sentinel"));
    let _ = std::fs::remove_dir_all(&moved);
}

#[test]
fn c6_lexical_rejections_over_the_wire() {
    let root = TempRoot::new("c6");
    root.write("a", b"x\n");
    let mut h = WsHarness::start(&root.path, GatedFs::new());
    let bad = [
        "../a",
        "./a",
        "a/..",
        "/a",
        "a//b",
        "a/",
        &vec!["a"; 65].join("/"),
        &"x".repeat(256),
        &format!("{}/{}", "y".repeat(255), "y".repeat(769)),
    ];
    for (i, p) in bad.iter().enumerate() {
        h.send(slice_call(i as u64 + 1, p));
        let r = h.recv_expect();
        assert!(is_error(&r, "invalid path"), "{p:?}: {r}");
    }
    h.send(slice_call(99, "a\u{0}b"));
    assert!(is_error(&h.recv_expect(), "invalid path"));
}

#[test]
fn c7_flipping_directory_never_yields_outside_content() {
    let root = TempRoot::new("c7");
    let outside = root.outside();
    std::fs::create_dir_all(outside.join("d")).unwrap();
    std::fs::write(outside.join("d/file"), b"OUTSIDE").unwrap();
    root.write("d/file", b"inside\n");
    let real = root.path.join("d_real");
    std::fs::rename(root.path.join("d"), &real).unwrap();
    std::fs::rename(&real, root.path.join("d")).unwrap();
    let flip_root = root.path.clone();
    let flip_outside = outside.join("d");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let flipper = std::thread::spawn(move || {
        let d = flip_root.join("d");
        let real = flip_root.join("d_real");
        let mut is_real = true;
        while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
            if is_real {
                let _ = std::fs::rename(&d, &real);
                let _ = std::os::unix::fs::symlink(&flip_outside, &d);
            } else {
                let _ = std::fs::remove_file(&d);
                let _ = std::fs::rename(&real, &d);
            }
            is_real = !is_real;
        }
        if !is_real {
            let _ = std::fs::remove_file(&d);
            let _ = std::fs::rename(&real, &d);
        }
    });
    let mut h = WsHarness::start(&root.path, GatedFs::new());
    let end = Instant::now() + Duration::from_secs(2);
    let mut i = 0u64;
    let mut inside = 0;
    let mut rejected = 0;
    while Instant::now() < end {
        i += 1;
        h.send(slice_call(i, "d/file"));
        let r = h.recv_expect();
        let t = text(&r);
        assert!(!t.contains("OUTSIDE"), "outside content leaked: {r}");
        if t == "1\tinside" {
            inside += 1;
        } else {
            assert!(
                t == "invalid path" || t == "not found" || t == "not a directory",
                "{r}"
            );
            rejected += 1;
        }
    }
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    flipper.join().unwrap();
    assert!(
        inside > 0 && rejected > 0,
        "inside {inside} rejected {rejected}"
    );
}

#[test]
fn c8_move_out_and_create_is_readable_only_through_the_held_descriptor() {
    let root = TempRoot::new("c8");
    let outside = root.outside();
    root.mkdir("a/b");
    root.write("other", b"x\n");
    let fs = GatedFs::new();
    let hold = fs.hold_after(Call::OpenDir, Some(b"a"));
    let mut h = WsHarness::start(&root.path, fs.clone());
    h.send(slice_call(1, "a/b/file"));
    hold.wait_engaged(Duration::from_secs(3)).expect("engaged");
    std::fs::rename(root.path.join("a"), outside.join("a")).unwrap();
    std::fs::write(outside.join("a/b/file"), b"S1\n").unwrap();
    hold.release();
    let r = h.recv_expect();
    // Documented retained-directory behaviour: the held descriptor serves the new content.
    assert_eq!(text(&r), "1\tS1", "{r}");
    assert!(h.wait_until(Duration::from_secs(2), |_, p| {
        p.job_descriptors_open == 0 && p.jobs_running == 0
    }));
    // New jobs resolve from the root and no longer see `a`.
    h.send(slice_call(2, "a/b/file"));
    assert!(is_error(&h.recv_expect(), "not found"));
    h.send(ws_call(json!(3), "workspace_outline", json!({})));
    let out = text(&h.recv_expect());
    assert!(!out.contains("d a"), "{out}");
    // Search variant: a directory held during its visit lists content created after the move.
    root.mkdir("s/t");
    root.write("s/t/keep.txt", b"needle\n");
    let hold2 = fs.hold_after(Call::OpenDir, Some(b"t"));
    h.send(ws_call(
        json!(4),
        "text_search",
        json!({"query":"needle","path":"s"}),
    ));
    hold2.wait_engaged(Duration::from_secs(3)).expect("engaged");
    std::fs::rename(root.path.join("s/t"), outside.join("t")).unwrap();
    std::fs::write(outside.join("t/new.txt"), b"needle NEW\n").unwrap();
    hold2.release();
    let r = h.recv_expect();
    let t = text(&r);
    assert!(t.contains("new.txt") && t.contains("keep.txt"), "{t}");
    h.send(ws_call(
        json!(5),
        "text_search",
        json!({"query":"needle","path":"s"}),
    ));
    let r2 = h.recv_expect();
    assert_eq!(meta(&r2)["matches"], 0);
    assert_eq!(meta(&r2)["dirs_visited"], 1);
}

#[test]
fn c9_fifo_is_classified_and_never_opened() {
    let root = TempRoot::new("c9");
    root.mkdir("sub");
    root.write("sub/t.txt", b"needle\n");
    let status = std::process::Command::new("mkfifo")
        .arg(root.path.join("pipe"))
        .status()
        .unwrap();
    assert!(status.success());
    let fs = GatedFs::new();
    let mut h = WsHarness::start(&root.path, fs.clone());
    let t0 = Instant::now();
    h.send(slice_call(1, "pipe"));
    let r = h.recv_expect();
    assert!(is_error(&r, "not a file"), "{r}");
    assert!(t0.elapsed() < Duration::from_millis(500));
    let opens_before = fs.calls(Call::OpenFile);
    h.send(ws_call(json!(2), "workspace_outline", json!({})));
    let out = text(&h.recv_expect());
    assert!(out.contains("o pipe"), "{out}");
    h.send(ws_call(json!(3), "text_search", json!({"query":"needle"})));
    let r = h.recv_expect();
    assert_eq!(meta(&r)["files_skipped"]["other"], 1);
    assert_eq!(meta(&r)["matches"], 1);
    // Exactly one file was opened across outline and search: sub/t.txt.
    assert_eq!(fs.calls(Call::OpenFile) - opens_before, 1);
}

#[test]
fn c10_replacement_between_classification_and_open_is_rejected_quickly() {
    let root = TempRoot::new("c10");
    root.write("file", b"inside\n");
    root.mkdir("d");
    root.write("d/file", b"inside\n");
    root.write("dir2file", b"x\n");
    let fs = GatedFs::new();
    let hold = fs.hold_after(Call::StatAt, Some(b"file"));
    let mut h = WsHarness::start(&root.path, fs.clone());
    h.send(slice_call(1, "file"));
    hold.wait_engaged(Duration::from_secs(3)).expect("engaged");
    std::fs::remove_file(root.path.join("file")).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(root.path.join("file"))
            .status()
            .unwrap()
            .success()
    );
    let released = Instant::now();
    hold.release();
    let r = h.recv_expect();
    assert!(is_error(&r, "not a file"), "{r}");
    assert!(released.elapsed() < Duration::from_millis(500));
    // File replaced by a directory.
    let hold = fs.hold_after(Call::StatAt, Some(b"dir2file"));
    h.send(slice_call(2, "dir2file"));
    hold.wait_engaged(Duration::from_secs(3)).expect("engaged");
    std::fs::remove_file(root.path.join("dir2file")).unwrap();
    std::fs::create_dir(root.path.join("dir2file")).unwrap();
    hold.release();
    assert!(is_error(&h.recv_expect(), "not a file"));
    // Directory replaced by a file.
    let hold = fs.hold_after(Call::StatAt, Some(b"d"));
    h.send(slice_call(3, "d/file"));
    hold.wait_engaged(Duration::from_secs(3)).expect("engaged");
    std::fs::remove_dir_all(root.path.join("d")).unwrap();
    std::fs::write(root.path.join("d"), b"now a file").unwrap();
    hold.release();
    assert!(is_error(&h.recv_expect(), "not a directory"));
}

#[test]
fn c11_sockets_are_not_files_even_when_raced() {
    let root = TempRoot::new("c11");
    let _listener = std::os::unix::net::UnixListener::bind(root.path.join("sock")).unwrap();
    root.write("later", b"x\n");
    let fs = GatedFs::new();
    let hold = fs.hold_after(Call::StatAt, Some(b"later"));
    let mut h = WsHarness::start(&root.path, fs);
    h.send(slice_call(1, "sock"));
    assert!(is_error(&h.recv_expect(), "not a file"));
    h.send(slice_call(2, "later"));
    hold.wait_engaged(Duration::from_secs(3)).expect("engaged");
    std::fs::remove_file(root.path.join("later")).unwrap();
    let _l2 = std::os::unix::net::UnixListener::bind(root.path.join("later")).unwrap();
    hold.release();
    let r = h.recv_expect();
    assert!(is_error(&r, "not a file"), "{r}");
}

#[test]
fn c12_unknown_entry_types_fall_back_to_classification() {
    let root = TempRoot::new("c12");
    root.write("f.txt", b"abc\n");
    root.mkdir("d");
    root.symlink("f.txt", "l");
    let fs = GatedFs::new();
    fs.set_force_unknown(true);
    let mut h = WsHarness::start(&root.path, fs.clone());
    h.send(ws_call(json!(1), "workspace_outline", json!({})));
    let out = text(&h.recv_expect());
    assert_eq!(out, "d d\nf f.txt 4\nl l");
    assert!(fs.calls(Call::StatAt) >= 3);
}
