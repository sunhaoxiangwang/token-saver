//! Workspace contract tests: bounded construction (F03), cutting and partial lines, generated
//! paths, metadata maxima, traversal bounds, fixed buffer capacities, descriptor accounting, and
//! output-schema validity, run directly on the calling thread through the test seam.

mod common;

use std::os::fd::AsFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::ws::{Call, GatedFs, TempRoot, root_fd};
use serde_json::{Value, json};
use token_saver::diag::{Diag, JobKind};
use token_saver::limits;
use token_saver::supervisor::Shared;
use token_saver::workspace::confine::FileSystem;
use token_saver::workspace::render::{
    SCAFFOLD_BYTES, TRAILER_MAX, escaped_len, meta_line, text_budget,
};
use token_saver::workspace::worker::{Buffers, metadata_max, run_job_direct};
use token_saver::workspace::{Args, parse_args};
use tokio_util::sync::CancellationToken;

struct Run {
    result: Value,
    size: usize,
    stop: &'static str,
    descriptors: usize,
}

fn args_of(tool: &str, v: Value) -> Args {
    parse_args(tool, v.as_object()).expect("valid arguments").1
}

fn run_with(
    root: &TempRoot,
    fs: Arc<dyn FileSystem>,
    tool: &str,
    arguments: Value,
    buffers: &mut Buffers,
    deadline: Instant,
) -> Run {
    let shared = Arc::new(Shared::new(CancellationToken::new(), Diag::disabled()));
    let fd = root_fd(&root.path);
    let args = args_of(tool, arguments);
    let before = buffers.capacities();
    let (result, stop, descriptors) = run_job_direct(
        fs,
        shared,
        fd.as_fd(),
        &args,
        buffers,
        deadline,
        &Diag::disabled(),
    )
    .expect("job ran to a result");
    assert_eq!(buffers.capacities(), before, "buffer capacities changed");
    let bytes = serde_json::to_vec(&result).unwrap();
    assert!(
        bytes.len() <= limits::MAX_WORKSPACE_RESULT_BYTES,
        "result {} bytes",
        bytes.len()
    );
    Run {
        result: serde_json::to_value(&result).unwrap(),
        size: bytes.len(),
        stop,
        descriptors,
    }
}

fn run(root: &TempRoot, tool: &str, arguments: Value, buffers: &mut Buffers) -> Run {
    run_with(
        root,
        Arc::new(GatedFs::new()),
        tool,
        arguments,
        buffers,
        Instant::now() + limits::PROCESSING,
    )
}

fn full_text(r: &Run) -> &str {
    r.result["content"][0]["text"].as_str().unwrap()
}

fn text(r: &Run) -> String {
    common::ws::content_of(full_text(r))
}

fn meta(r: &Run) -> Value {
    assert!(
        r.result.get("structuredContent").is_none(),
        "text-only results"
    );
    common::ws::meta_of(full_text(r))
}

fn validate(tool: &str, r: &Run) {
    let schema = match tool {
        "workspace_outline" => token_saver::workspace::outline_meta_schema(),
        "file_slice" => token_saver::workspace::slice_meta_schema(),
        _ => token_saver::workspace::search_meta_schema(),
    };
    let validator = jsonschema::validator_for(&Value::Object((*schema).clone())).unwrap();
    let m = meta(r);
    let errors: Vec<String> = validator.iter_errors(&m).map(|e| e.to_string()).collect();
    assert!(
        errors.is_empty(),
        "{tool} metadata violates schema: {errors:?}"
    );
}

#[test]
fn review_escaped_content_case_is_bounded_without_building_the_candidate() {
    let root = TempRoot::new("escaped");
    let line = format!("x{}\n", "\u{1}".repeat(199));
    let content = line.repeat(100);
    assert_eq!(content.len(), 20_100);
    root.write("big.txt", content.as_bytes());
    let mut b = Buffers::new();
    let r = run(
        &root,
        "text_search",
        json!({"query":"x","max_matches":100}),
        &mut b,
    );
    assert_eq!(r.stop, "result_cap");
    assert_eq!(meta(&r)["stop_reason"], "result_cap");
    let m = meta(&r)["matches"].as_u64().unwrap();
    assert!((1..10).contains(&m), "matches {m}");
    assert!(text(&r).ends_with("[truncated: result_cap]"));
    assert_eq!(r.descriptors, 0);
    validate("text_search", &r);
    // With maximum context the same bound holds.
    let r2 = run(
        &root,
        "text_search",
        json!({"query":"x","max_matches":100,"context_lines":2}),
        &mut b,
    );
    assert_eq!(r2.stop, "result_cap");
    assert!(r2.size <= limits::MAX_WORKSPACE_RESULT_BYTES);
    validate("text_search", &r2);
}

#[test]
fn slice_of_control_character_lines_admits_four_lines() {
    let root = TempRoot::new("ctl");
    let line = format!("{}\n", "\u{1}".repeat(400));
    root.write("ctl.txt", line.repeat(10).as_bytes());
    let mut b = Buffers::new();
    let r = run(
        &root,
        "file_slice",
        json!({"path":"ctl.txt","max_lines":10}),
        &mut b,
    );
    assert_eq!(r.stop, "result_cap");
    assert_eq!(meta(&r)["returned_lines"], 4);
    assert_eq!(meta(&r)["end_line"], 4);
    assert_eq!(meta(&r)["at_eof"], false);
    assert_eq!(meta(&r)["truncated"], true);
    validate("file_slice", &r);
}

#[test]
fn dense_result_fills_the_budget_and_refuses_the_next_unit() {
    let root = TempRoot::new("dense");
    let line = format!("{}\n", "a".repeat(100));
    root.write("dense.txt", line.repeat(400).as_bytes());
    let mut b = Buffers::new();
    let r = run(
        &root,
        "file_slice",
        json!({"path":"dense.txt","max_lines":400,"max_bytes":12288}),
        &mut b,
    );
    assert_eq!(r.stop, "result_cap");
    assert!(r.size <= limits::MAX_WORKSPACE_RESULT_BYTES);
    assert!(r.size >= 11_000, "{}", r.size);
    let returned = meta(&r)["returned_lines"].as_u64().unwrap();
    let budget = text_budget(metadata_max(JobKind::Slice));
    let cost = |n: u64| escaped_len(&format!("{n}\t{}", "a".repeat(100))) + 2;
    let used: usize = (1..=returned).map(cost).sum();
    assert!(
        used <= budget && used + cost(returned + 1) > budget,
        "used {used} next {} budget {budget}",
        cost(returned + 1)
    );
    validate("file_slice", &r);
}

#[test]
fn generated_paths_are_bounded_like_arguments() {
    let root = TempRoot::new("deep");
    let a = "y".repeat(255);
    let b2 = "y".repeat(254);
    // Argument path of 513 bytes with three components.
    let arg = format!("{a}/{b2}/xy");
    assert_eq!(arg.len(), 513);
    let d1 = "d".repeat(255);
    let d2 = "e".repeat(254);
    // depth-1 entry path: 513 + 1 + 255 = 769; depth-2: 1,024; depth-3: over the limit.
    // Created component-wise because the absolute path exceeds the platform PATH_MAX.
    let deepest = root.mkdir_deep(&[&a, &b2, "xy", &d1, &d2, "deeper"]);
    TempRoot::write_at(&deepest, "f.txt", b"needle here\n");
    let d2fd = root.mkdir_deep(&[&a, &b2, "xy", &d1, &d2]);
    TempRoot::write_at(&d2fd, "g.txt", b"needle too\n");
    let mut b = Buffers::new();
    let r = run(
        &root,
        "workspace_outline",
        json!({"path": arg, "depth": 3, "max_entries": 200}),
        &mut b,
    );
    assert_eq!(r.stop, "complete");
    let t = text(&r);
    assert!(t.contains(&format!("d {arg}/{d1}\n")));
    assert!(t.contains(&format!("d {arg}/{d1}/{d2}")));
    assert!(
        !t.contains("deeper"),
        "depth-3 entries exceed 1,024 bytes: {t}"
    );
    assert_eq!(meta(&r)["too_deep"], 2);
    validate("workspace_outline", &r);
    let s = run(
        &root,
        "text_search",
        json!({"query":"needle","path": arg}),
        &mut b,
    );
    assert_eq!(s.stop, "complete");
    assert_eq!(meta(&s)["files_skipped"]["too_deep"], 2);
    assert_eq!(meta(&s)["matches"], 0);
    validate("text_search", &s);
    // 65 components: too deep for search even when short.
    let chain = vec!["a"; 66].join("/");
    root.mkdir(&chain);
    root.write(&format!("{chain}/n.txt"), b"needle\n");
    let s2 = run(
        &root,
        "text_search",
        json!({"query":"needle","path":"a"}),
        &mut b,
    );
    assert_eq!(meta(&s2)["files_skipped"]["too_deep"], 1);
    assert_eq!(meta(&s2)["matches"], 0);
}

#[test]
fn control_byte_names_render_escaped_and_stay_bounded() {
    let root = TempRoot::new("ctlnames");
    for i in 0..20 {
        let name = format!("{}{i:02}", "\u{1}".repeat(200));
        root.write(&name, b"content\n");
    }
    let mut b = Buffers::new();
    let r = run(&root, "workspace_outline", json!({}), &mut b);
    assert_eq!(r.stop, "result_cap");
    assert!(r.size <= limits::MAX_WORKSPACE_RESULT_BYTES);
    let listed = meta(&r)["entries_listed"].as_u64().unwrap();
    assert!((5..20).contains(&listed), "{listed}");
    assert!(text(&r).contains("\\x01\\x01"));
    validate("workspace_outline", &r);
    let s = run(&root, "text_search", json!({"query":"content"}), &mut b);
    assert_eq!(s.stop, "result_cap");
    assert!(text(&s).contains("== \\x01"));
}

#[test]
fn metadata_maxima_match_the_serializer_and_bound_real_metadata() {
    use token_saver::workspace::{outline, search, slice};
    for (kind, max) in [
        (JobKind::Outline, outline::max_metadata()),
        (JobKind::Slice, slice::max_metadata()),
        (JobKind::Search, search::max_metadata()),
    ] {
        assert_eq!(metadata_max(kind), escaped_len(&meta_line(&max)) + 2);
        assert!(metadata_max(kind) <= 800, "{kind:?} {}", metadata_max(kind));
    }
    let root = TempRoot::new("metamax");
    root.write("f.txt", b"one\ntwo\n");
    let mut b = Buffers::new();
    for (tool, args, kind) in [
        ("workspace_outline", json!({}), JobKind::Outline),
        ("file_slice", json!({"path":"f.txt"}), JobKind::Slice),
        ("text_search", json!({"query":"one"}), JobKind::Search),
    ] {
        let r = run(&root, tool, args, &mut b);
        let actual = escaped_len(full_text(&r).rsplit('\n').next().unwrap()) + 2;
        assert!(actual <= metadata_max(kind), "{tool}: {actual}");
    }
    const { assert!(SCAFFOLD_BYTES + TRAILER_MAX + 800 < limits::MAX_WORKSPACE_RESULT_BYTES) };
}

#[test]
fn partial_lines_report_unknown_remainders() {
    let root = TempRoot::new("partial");
    // A line whose end lies beyond the look-ahead limit.
    let mut long = vec![b'q'; 1_700 + limits::LINE_LOOKAHEAD_BYTES + 10];
    long.push(b'\n');
    long.extend_from_slice(b"second\n");
    root.write("long.txt", &long);
    let mut b = Buffers::new();
    let r = run(&root, "file_slice", json!({"path":"long.txt"}), &mut b);
    assert_eq!(r.stop, "line_too_long");
    assert_eq!(meta(&r)["last_line_partial"], true);
    assert_eq!(meta(&r)["returned_lines"], 1);
    assert_eq!(meta(&r)["lines_cut"], 1);
    assert!(text(&r).contains(" [+? chars]"), "{}", text(&r));
    assert!(text(&r).ends_with("[truncated: line_too_long]"));
    assert!(meta(&r).get("total_lines").is_none());
    validate("file_slice", &r);
    // A cut line whose end is reached: exact count.
    root.write(
        "cut.txt",
        format!("{}\nshort\n", "b".repeat(1_000)).as_bytes(),
    );
    let r2 = run(&root, "file_slice", json!({"path":"cut.txt"}), &mut b);
    assert_eq!(r2.stop, "complete");
    assert!(text(&r2).starts_with(&format!("1\t{} [+600 chars]\n2\tshort", "b".repeat(400))));
    assert_eq!(meta(&r2)["lines_cut"], 1);
    assert_eq!(meta(&r2)["at_eof"], true);
    assert_eq!(meta(&r2)["total_lines"], 2);
    // The deadline inside a long line: hold inside the second read until the deadline passes.
    let mut big = vec![b'z'; 200_000];
    big.push(b'\n');
    root.write("deadline.txt", &big);
    let fs = Arc::new(GatedFs::new());
    let hold = fs.hold_inside_nth(Call::Read, None, 2);
    let deadline = Instant::now() + Duration::from_millis(700);
    let fs2 = fs.clone();
    let releaser = std::thread::spawn(move || {
        hold.wait_engaged(Duration::from_secs(5)).expect("engaged");
        std::thread::sleep(Duration::from_millis(800));
        hold.release();
        let _ = fs2;
    });
    let r3 = run_with(
        &root,
        fs,
        "file_slice",
        json!({"path":"deadline.txt"}),
        &mut b,
        deadline,
    );
    releaser.join().unwrap();
    assert_eq!(r3.stop, "time_budget");
    assert_eq!(meta(&r3)["last_line_partial"], true);
    assert!(text(&r3).contains("[+? chars]"));
    assert!(text(&r3).ends_with("[truncated: time_budget]"));
    validate("file_slice", &r3);
}

#[test]
fn slice_edge_cases_follow_the_contract() {
    let root = TempRoot::new("edges");
    root.write("empty.txt", b"");
    root.write("crlf.txt", b"a\r\nb\r\n");
    root.write("mixed.txt", b"a\r\nb\nc");
    root.write("lossy.txt", &[b'o', b'k', 0xff, b'\n']);
    root.write("bin.dat", &[1, 2, 0, 4, b'\n']);
    let mut b = Buffers::new();
    let r = run(&root, "file_slice", json!({"path":"empty.txt"}), &mut b);
    assert_eq!(r.stop, "start_past_eof");
    assert_eq!(text(&r), "[no lines: start_line 1 is past EOF]");
    assert_eq!(meta(&r)["at_eof"], true);
    assert_eq!(meta(&r)["line_endings"], "none");
    assert_eq!(meta(&r)["total_lines"], 0);
    validate("file_slice", &r);
    let r = run(&root, "file_slice", json!({"path":"crlf.txt"}), &mut b);
    assert_eq!(text(&r), "1\ta\n2\tb");
    assert_eq!(meta(&r)["line_endings"], "crlf");
    assert_eq!(meta(&r)["at_eof"], true);
    assert_eq!(meta(&r)["stop_reason"], "complete");
    let r = run(&root, "file_slice", json!({"path":"mixed.txt"}), &mut b);
    assert_eq!(text(&r), "1\ta\n2\tb\n3\tc");
    assert_eq!(meta(&r)["line_endings"], "mixed");
    assert_eq!(meta(&r)["total_lines"], 3);
    let r = run(&root, "file_slice", json!({"path":"lossy.txt"}), &mut b);
    assert_eq!(meta(&r)["lossy"], true);
    assert_eq!(text(&r), "1\tok\u{fffd}");
    let r = run(&root, "file_slice", json!({"path":"bin.dat"}), &mut b);
    assert_eq!(r.stop, "binary");
    assert_eq!(text(&r), "[binary file, 5 bytes]");
    assert_eq!(meta(&r)["encoding"], "binary");
    assert_eq!(meta(&r)["returned_lines"], 0);
    validate("file_slice", &r);
    let r = run(
        &root,
        "file_slice",
        json!({"path":"mixed.txt","start_line":9}),
        &mut b,
    );
    assert_eq!(r.stop, "start_past_eof");
    assert_eq!(text(&r), "[no lines: start_line 9 is past EOF]");
    // max_bytes: the first line always returns (cut), a later line that would exceed stops.
    root.write(
        "bytes.txt",
        format!("{}\n{}\n", "c".repeat(300), "d".repeat(300)).as_bytes(),
    );
    let r = run(
        &root,
        "file_slice",
        json!({"path":"bytes.txt","max_bytes":256}),
        &mut b,
    );
    assert_eq!(r.stop, "max_bytes");
    assert_eq!(meta(&r)["returned_lines"], 1);
    assert!(text(&r).starts_with(&format!("1\t{} [+44 chars]", "c".repeat(256))));
    let r = run(
        &root,
        "file_slice",
        json!({"path":"bytes.txt","max_bytes":400}),
        &mut b,
    );
    assert_eq!(r.stop, "max_bytes");
    assert_eq!(meta(&r)["returned_lines"], 1);
    assert_eq!(meta(&r)["lines_cut"], 0);
}

#[test]
fn search_output_is_grep_style_with_headers_context_and_separators() {
    let root = TempRoot::new("grep");
    root.write(
        "a.txt",
        b"zero\nNeedle one\ntwo\nthree\nfour\nneedle five\nsix\n",
    );
    root.write("b.txt", b"nothing\n");
    root.mkdir("sub");
    root.write("sub/c.txt", b"NEEDLE\n");
    let mut b = Buffers::new();
    let r = run(
        &root,
        "text_search",
        json!({"query":"needle","context_lines":1}),
        &mut b,
    );
    assert_eq!(r.stop, "complete");
    assert_eq!(
        text(&r),
        "== a.txt\n1-zero\n2:Needle one\n3-two\n--\n5-four\n6:needle five\n7-six\n== sub/c.txt\n1:NEEDLE"
    );
    assert_eq!(meta(&r)["matches"], 3);
    assert_eq!(meta(&r)["files_matched"], 2);
    assert_eq!(meta(&r)["files_scanned"], 3);
    validate("text_search", &r);
    let r = run(
        &root,
        "text_search",
        json!({"query":"needle","case_sensitive":true}),
        &mut b,
    );
    assert_eq!(text(&r), "== a.txt\n6:needle five");
    let r = run(
        &root,
        "text_search",
        json!({"query":"needle","max_matches":1}),
        &mut b,
    );
    assert_eq!(r.stop, "max_matches");
    assert_eq!(text(&r), "== a.txt\n2:Needle one\n[truncated: max_matches]");
    let r = run(
        &root,
        "text_search",
        json!({"query":"NEEDLE","path":"sub/c.txt"}),
        &mut b,
    );
    assert_eq!(text(&r), "== sub/c.txt\n1:NEEDLE");
    assert_eq!(meta(&r)["files_scanned"], 1);
}

#[test]
fn traversal_bounds_are_enforced() {
    let root = TempRoot::new("bounds");
    // Five directories of 5,000 hidden files (raw entries that are never scanned) plus ten
    // visible files in the first: 25,010 raw entries.
    for d in 0..5 {
        root.mkdir(&format!("d{d}"));
        for f in 0..5_000 {
            root.write(&format!("d{d}/.h{f:05}"), b"");
        }
    }
    for f in 0..10 {
        root.write(&format!("d0/v{f:02}.txt"), b"needle\n");
    }
    let mut b = Buffers::new();
    // 5,010 entries in d0: the listing is truncated at 5,000 and the raw budget is spent.
    let r = run(
        &root,
        "workspace_outline",
        json!({"path":"d0","max_entries":200}),
        &mut b,
    );
    assert_eq!(r.stop, "raw_entry_limit", "{}", meta(&r));
    assert_eq!(meta(&r)["dirs_truncated"], 1);
    assert!(meta(&r)["hidden_skipped"].as_u64().unwrap() >= 4_990);
    validate("workspace_outline", &r);
    // Search: 5 + 5,000 + 5,000 + 5,000 + 4,995 raw entries reach max_entries before any
    // file bound; only the visible files are scanned.
    let r = run(&root, "text_search", json!({"query":"needle"}), &mut b);
    assert_eq!(r.stop, "max_entries", "{}", meta(&r));
    assert!(meta(&r)["files_scanned"].as_u64().unwrap() <= 10);
    // 20,000 raw entries less the five directory entries and the visible files read.
    assert!(meta(&r)["files_skipped"]["hidden"].as_u64().unwrap() >= 19_980);
    validate("text_search", &r);
    // Visible files only: max_files when a directory holds more than 2,000 of them.
    for f in 0..2_005 {
        root.write(&format!("d5/m{f:04}.txt"), b"");
    }
    let r = run(
        &root,
        "text_search",
        json!({"query":"needle","path":"d5"}),
        &mut b,
    );
    assert_eq!(r.stop, "max_files", "{}", meta(&r));
    assert_eq!(meta(&r)["files_scanned"], 2_000);
}

#[test]
fn search_max_dirs_bound() {
    let root = TempRoot::new("maxdirs");
    for d in 0..4_100 {
        root.mkdir(&format!("x{d:04}"));
    }
    let mut b = Buffers::new();
    let r = run(&root, "text_search", json!({"query":"nomatch"}), &mut b);
    assert_eq!(r.stop, "max_dirs");
    validate("text_search", &r);
}

#[test]
fn argument_validation_yields_fixed_errors() {
    let cases: Vec<(&str, Value, &str)> = vec![
        ("file_slice", json!({}), "missing path"),
        ("file_slice", json!({"path":""}), "invalid path"),
        ("file_slice", json!({"path":"/etc/passwd"}), "invalid path"),
        ("file_slice", json!({"path":"a/../b"}), "invalid path"),
        (
            "file_slice",
            json!({"path":"a","max_lines":0}),
            "invalid argument",
        ),
        (
            "file_slice",
            json!({"path":"a","max_lines":401}),
            "invalid argument",
        ),
        (
            "file_slice",
            json!({"path":"a","max_bytes":255}),
            "invalid argument",
        ),
        (
            "file_slice",
            json!({"path":"a","start_line":1.5}),
            "invalid argument",
        ),
        (
            "file_slice",
            json!({"path":"a","extra":1}),
            "unexpected argument",
        ),
        (
            "file_slice",
            json!({"path":"a","zzz":"x".repeat(4_100)}),
            "arguments too large",
        ),
        ("workspace_outline", json!({"depth":4}), "invalid argument"),
        ("workspace_outline", json!({"path":5}), "invalid path"),
        (
            "workspace_outline",
            json!({"include_hidden":"yes"}),
            "invalid argument",
        ),
        ("text_search", json!({}), "invalid argument"),
        ("text_search", json!({"query":""}), "invalid argument"),
        (
            "text_search",
            json!({"query":"x".repeat(129)}),
            "invalid argument",
        ),
        ("text_search", json!({"query":"a\tb"}), "invalid argument"),
        (
            "text_search",
            json!({"query":"ok","context_lines":3}),
            "invalid argument",
        ),
        (
            "text_search",
            json!({"query":"ok","max_matches":101}),
            "invalid argument",
        ),
    ];
    for (tool, args, expected) in cases {
        let err = parse_args(tool, args.as_object()).err();
        assert_eq!(err, Some(expected), "{tool} {args}");
    }
    assert!(parse_args("workspace_outline", None).is_ok());
    assert_eq!(parse_args("file_slice", None).err(), Some("missing path"));
    let ok = parse_args("text_search", json!({"query":"é".repeat(128)}).as_object()).unwrap();
    assert!(matches!(ok.1, Args::Search(_)));
}

#[test]
fn review_f04_every_read_step_is_at_most_64_kib() {
    let root = TempRoot::new("readstep");
    let line = format!("{}\n", "r".repeat(99));
    root.write("big.txt", line.repeat(10_000).as_bytes());
    assert!(std::fs::metadata(root.path.join("big.txt")).unwrap().len() <= 1_048_576);
    let fs = Arc::new(GatedFs::new());
    let mut b = Buffers::new();
    let r = run_with(
        &root,
        fs.clone(),
        "text_search",
        json!({"query":"rrrr","max_matches":5}),
        &mut b,
        Instant::now() + limits::PROCESSING,
    );
    assert_eq!(r.stop, "max_matches");
    assert!(
        fs.max_read_len() <= limits::READ_STEP_BYTES,
        "{}",
        fs.max_read_len()
    );
    assert!(
        fs.calls(Call::Read) >= 15,
        "a 1 MB file needs many 64 KiB steps"
    );
    let r2 = run_with(
        &root,
        fs.clone(),
        "file_slice",
        json!({"path":"big.txt","start_line":9_990}),
        &mut b,
        Instant::now() + limits::PROCESSING,
    );
    assert_eq!(meta(&r2)["returned_lines"], 11);
    assert!(fs.max_read_len() <= limits::READ_STEP_BYTES);
}

#[test]
fn review_f05_metadata_agrees_with_returned_content() {
    let root = TempRoot::new("f05");
    let mut b = Buffers::new();
    // (a) files_matched counts a file whose match was reported when the result cap stops the scan.
    let line = format!("x{}\n", "\u{1}".repeat(199));
    root.write("big.txt", line.repeat(100).as_bytes());
    let r = run(
        &root,
        "text_search",
        json!({"query":"x","max_matches":100}),
        &mut b,
    );
    assert_eq!(r.stop, "result_cap");
    assert_eq!(meta(&r)["files_matched"], 1);
    assert!(meta(&r)["matches"].as_u64().unwrap() >= 1);
    root.write("two.txt", b"x one\nx two\n");
    let r = run(
        &root,
        "text_search",
        json!({"query":"x","max_matches":1,"path":"two.txt"}),
        &mut b,
    );
    assert_eq!(r.stop, "max_matches");
    assert_eq!(meta(&r)["files_matched"], 1);
    assert_eq!(meta(&r)["matches"], 1);
    // (b) lines_cut counts only admitted lines.
    root.write(
        "ctl6.txt",
        format!("{}\n", "\u{1}".repeat(500)).repeat(6).as_bytes(),
    );
    let r = run(&root, "file_slice", json!({"path":"ctl6.txt"}), &mut b);
    assert_eq!(r.stop, "result_cap");
    assert_eq!(meta(&r)["returned_lines"], 4);
    assert_eq!(meta(&r)["lines_cut"], 4);
    assert_eq!(
        text(&r)
            .lines()
            .filter(|l| l.contains("[+100 chars]"))
            .count(),
        4
    );
    // (c) a multibyte character split at the storage boundary is not lossy and is counted.
    root.write(
        "split.txt",
        format!("{}\u{e9}\nnext\n", "a".repeat(1_603)).as_bytes(),
    );
    let r = run(&root, "file_slice", json!({"path":"split.txt"}), &mut b);
    assert_eq!(r.stop, "complete");
    assert_eq!(meta(&r)["lossy"], false);
    assert_eq!(meta(&r)["lines_cut"], 1);
    assert!(
        text(&r).starts_with(&format!("1\t{} [+1204 chars]\n2\tnext", "a".repeat(400))),
        "{}",
        text(&r)
    );
    // A genuinely invalid byte inside the stored part is still lossy.
    let mut bad = vec![b'b'; 100];
    bad.push(0xff);
    bad.extend_from_slice(b"c\n");
    root.write("bad.txt", &bad);
    let r = run(&root, "file_slice", json!({"path":"bad.txt"}), &mut b);
    assert_eq!(meta(&r)["lossy"], true);
}
