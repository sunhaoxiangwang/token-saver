//! `text_search`: literal search over text files below a directory, grep-style output with one
//! header per file. Each file is read completely into the worker's 1 MiB buffer, so the end of
//! every match line is known.

use std::collections::VecDeque;
use std::os::fd::BorrowedFd;
use std::time::Instant;

use serde_json::{Value, json};

use super::SearchArgs;
use super::confine::{DirHandle, Fail, Kind, classify, list_dir, open_regular, walk_dir};
use super::outline::{arena_path, join};
use super::policy::{
    ERR_DENIED_PATH, ERR_INVALID_PATH, ERR_NOT_A_FILE, is_denied, is_excluded_dir, is_hidden,
    render_name, within_path_limits,
};
use super::render::{TextBuilder, cut_chars, text_budget};
use super::worker::{Buffers, Built, Node, Stepper, Stop, metadata_max};
use crate::diag::JobKind;
use crate::limits;

#[derive(Default)]
struct Counters {
    matches: u64,
    files_scanned: u64,
    files_matched: u64,
    binary: u64,
    too_large: u64,
    denied: u64,
    symlink: u64,
    hidden: u64,
    excluded: u64,
    unreadable: u64,
    other: u64,
    too_deep: u64,
    dirs_visited: u64,
    dirs_truncated: u64,
    bytes_scanned: u64,
    raw_entries: usize,
}

fn line_matches(line: &[u8], needle: &[u8], case_sensitive: bool) -> bool {
    if needle.is_empty() || needle.len() > line.len() {
        return false;
    }
    if case_sensitive {
        line.windows(needle.len()).any(|w| w == needle)
    } else {
        line.windows(needle.len()).any(|w| {
            w.iter()
                .zip(needle)
                .all(|(a, b)| a.to_ascii_lowercase() == *b)
        })
    }
}

pub fn run(
    a: &SearchArgs,
    st: &mut Stepper,
    root: BorrowedFd<'_>,
    b: &mut Buffers,
) -> Result<Built, Stop> {
    b.text.reset(text_budget(metadata_max(JobKind::Search)));
    st.set_soft_deadline(Instant::now() + limits::SEARCH_TIME_BUDGET);
    let needle: Vec<u8> = if a.case_sensitive {
        a.query.as_bytes().to_vec()
    } else {
        a.query.to_ascii_lowercase().into_bytes()
    };
    let mut c = Counters::default();
    let base: Vec<&[u8]> = a.comps.iter().map(|s| s.as_bytes()).collect();
    let outcome = start(a, &base, &needle, st, root, b, &mut c);
    let stop_reason = match outcome {
        Ok(reason) => reason,
        Err(Fail::Tool(message)) => return Ok(Built::Error(message)),
        Err(Fail::Stop(Stop::Deadline)) => "time_budget",
        Err(Fail::Stop(s)) => return Err(s),
    };
    let truncated = stop_reason != "complete";
    let text = b
        .text
        .finish(if truncated { Some(stop_reason) } else { None });
    let meta = json!({
        "query_chars": a.query.chars().count(),
        "matches": c.matches,
        "files_scanned": c.files_scanned,
        "files_matched": c.files_matched,
        "files_skipped": {
            "binary": c.binary,
            "too_large": c.too_large,
            "denied": c.denied,
            "symlink": c.symlink,
            "hidden": c.hidden,
            "excluded": c.excluded,
            "unreadable": c.unreadable,
            "other": c.other,
            "too_deep": c.too_deep,
        },
        "dirs_visited": c.dirs_visited,
        "dirs_truncated": c.dirs_truncated,
        "bytes_scanned": c.bytes_scanned,
        "truncated": truncated,
        "stop_reason": stop_reason,
    });
    Ok(Built::Result {
        text,
        meta,
        stop_reason,
    })
}

fn start(
    a: &SearchArgs,
    base: &[&[u8]],
    needle: &[u8],
    st: &mut Stepper,
    root: BorrowedFd<'_>,
    b: &mut Buffers,
    c: &mut Counters,
) -> Result<&'static str, Fail> {
    if let Some((name, parents)) = base.split_last() {
        let dir = walk_dir(st, root, parents)?;
        let meta = classify(st, dir.fd(), name)?;
        match meta.kind {
            Kind::Symlink => return Err(Fail::Tool(ERR_INVALID_PATH)),
            Kind::Other | Kind::Unknown => return Err(Fail::Tool(ERR_NOT_A_FILE)),
            Kind::Regular => {
                if is_denied(name) {
                    return Err(Fail::Tool(ERR_DENIED_PATH));
                }
                let path = join(base);
                return match scan_file(a, needle, st, dir.fd(), name, &path, b, c)? {
                    Some(stop) => Ok(stop),
                    None => Ok("complete"),
                };
            }
            Kind::Directory => {}
        }
        drop(dir);
    }
    traverse(a, base, needle, st, root, b, c)
}

fn traverse(
    a: &SearchArgs,
    base: &[&[u8]],
    needle: &[u8],
    st: &mut Stepper,
    root: BorrowedFd<'_>,
    b: &mut Buffers,
    c: &mut Counters,
) -> Result<&'static str, Fail> {
    b.arena.clear();
    b.arena.push(Node {
        parent: u32::MAX,
        depth: 0,
        name: Vec::new(),
    });
    let mut i = 0;
    while i < b.arena.len() {
        let names = arena_path(&b.arena, i);
        let mut full: Vec<&[u8]> = base.to_vec();
        full.extend(names.iter().map(|n| n.as_slice()));
        let dir: DirHandle<'_> = match walk_dir(st, root, &full) {
            Ok(d) => d,
            Err(Fail::Tool(message)) => {
                if i == 0 {
                    return Err(Fail::Tool(message));
                }
                c.unreadable += 1;
                i += 1;
                continue;
            }
            Err(Fail::Stop(s)) => return Err(Fail::Stop(s)),
        };
        let remaining = limits::SEARCH_MAX_ENTRIES.saturating_sub(c.raw_entries);
        if remaining == 0 {
            return Ok("max_entries");
        }
        let truncated = list_dir(
            st,
            dir.fd(),
            remaining.min(limits::DIR_ENTRY_LIMIT),
            &mut b.listing,
        )?;
        c.raw_entries += b.listing.len();
        c.dirs_visited += 1;
        if truncated {
            c.dirs_truncated += 1;
        }
        b.listing.sort_by(|x, y| x.name.cmp(&y.name));
        let prefix = join(&full);
        let components = full.len() + 1;
        // Entries are taken out of the listing buffer so the file scan can use the buffers.
        let entries = std::mem::take(&mut b.listing);
        let mut result: Result<Option<&'static str>, Fail> = Ok(None);
        for e in entries.iter() {
            if let Err(s) = st.check() {
                result = Err(Fail::Stop(s));
                break;
            }
            if is_hidden(&e.name) && !a.include_hidden {
                c.hidden += 1;
                continue;
            }
            let kind = if e.kind == Kind::Unknown {
                match classify(st, dir.fd(), &e.name) {
                    Ok(m) => m.kind,
                    Err(Fail::Tool(_)) => {
                        c.unreadable += 1;
                        continue;
                    }
                    Err(Fail::Stop(s)) => {
                        result = Err(Fail::Stop(s));
                        break;
                    }
                }
            } else {
                e.kind
            };
            let mut path = prefix.clone();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(&e.name);
            if !within_path_limits(path.len(), components) {
                c.too_deep += 1;
                continue;
            }
            match kind {
                Kind::Directory => {
                    if is_excluded_dir(&e.name) {
                        c.excluded += 1;
                    } else if b.arena.len() >= limits::SEARCH_MAX_DIRS {
                        result = Ok(Some("max_dirs"));
                        break;
                    } else {
                        b.arena.push(Node {
                            parent: i as u32,
                            depth: b.arena[i].depth.saturating_add(1),
                            name: e.name.clone(),
                        });
                    }
                }
                Kind::Symlink => c.symlink += 1,
                Kind::Other | Kind::Unknown => c.other += 1,
                Kind::Regular => {
                    if is_denied(&e.name) {
                        c.denied += 1;
                        continue;
                    }
                    if c.files_scanned >= limits::SEARCH_MAX_FILES as u64 {
                        result = Ok(Some("max_files"));
                        break;
                    }
                    match scan_file(a, needle, st, dir.fd(), &e.name, &path, b, c) {
                        Ok(None) => {}
                        Ok(Some(stop)) => {
                            result = Ok(Some(stop));
                            break;
                        }
                        Err(f) => {
                            result = Err(f);
                            break;
                        }
                    }
                }
            }
        }
        b.listing = entries;
        match result {
            Ok(None) => {}
            Ok(Some(stop)) => return Ok(stop),
            Err(f) => return Err(f),
        }
        if c.raw_entries >= limits::SEARCH_MAX_ENTRIES && (truncated || i + 1 < b.arena.len()) {
            return Ok("max_entries");
        }
        i += 1;
    }
    Ok("complete")
}

/// Scans one regular file. Returns a stop reason when collection must end.
#[allow(clippy::too_many_arguments)]
fn scan_file(
    a: &SearchArgs,
    needle: &[u8],
    st: &mut Stepper,
    dir: BorrowedFd<'_>,
    name: &[u8],
    path: &[u8],
    b: &mut Buffers,
    c: &mut Counters,
) -> Result<Option<&'static str>, Fail> {
    let (file, size) = match open_regular(st, dir, name) {
        Ok(x) => x,
        Err(Fail::Tool(_)) => {
            c.unreadable += 1;
            return Ok(None);
        }
        Err(Fail::Stop(s)) => return Err(Fail::Stop(s)),
    };
    if size > limits::SEARCH_FILE_BYTES as u64 {
        c.too_large += 1;
        return Ok(None);
    }
    if c.bytes_scanned + size > limits::SEARCH_SCAN_BUDGET_BYTES as u64 {
        return Ok(Some("scan_budget"));
    }
    let mut filled = 0usize;
    let mut too_large = false;
    loop {
        if filled == b.file.len() {
            // Buffer full: one more read decides whether the file grew past the limit.
            let mut probe = [0u8; 1];
            match st.step(|fs| fs.read(&file, &mut probe))? {
                Ok(0) => {}
                Ok(_) => too_large = true,
                Err(_) => {
                    c.unreadable += 1;
                    return Ok(None);
                }
            }
            break;
        }
        let end = (filled + limits::READ_STEP_BYTES).min(b.file.len());
        match st.step(|fs| fs.read(&file, &mut b.file[filled..end]))? {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => {
                c.unreadable += 1;
                return Ok(None);
            }
        }
    }
    drop(file);
    if too_large {
        c.too_large += 1;
        return Ok(None);
    }
    c.bytes_scanned += filled as u64;
    let data = &b.file[..filled];
    if data[..filled.min(limits::BINARY_SNIFF_BYTES)].contains(&0) {
        c.binary += 1;
        return Ok(None);
    }
    c.files_scanned += 1;
    let rendered = render_name(path);
    let mut emitter = Emitter {
        text: &mut b.text,
        header: format!("== {rendered}"),
        header_pushed: false,
        last_emitted: None,
        prev: VecDeque::with_capacity(a.context_lines + 1),
        after_remaining: 0,
        matched: false,
    };
    let mut line_no: u64 = 0;
    let mut start = 0usize;
    while start <= data.len() {
        let end = data[start..]
            .iter()
            .position(|&c| c == b'\n')
            .map(|p| start + p);
        let (line_end, next) = match end {
            Some(e) => (e, e + 1),
            None => {
                if start == data.len() {
                    break;
                }
                (data.len(), data.len() + 1)
            }
        };
        line_no += 1;
        let mut line = &data[start..line_end];
        if let Some(s) = line.strip_suffix(b"\r") {
            line = s;
        }
        let mut stop: Option<&'static str> = None;
        if line_matches(line, needle, a.case_sensitive) {
            if !emitter.emit_match(line_no, line) {
                stop = Some("result_cap");
            } else {
                c.matches += 1;
                emitter.after_remaining = a.context_lines;
                if c.matches >= a.max_matches as u64 {
                    stop = Some("max_matches");
                }
            }
        } else if emitter.after_remaining > 0 {
            emitter.after_remaining -= 1;
            if !emitter.emit_context(line_no, line) {
                stop = Some("result_cap");
            }
        }
        if let Some(reason) = stop {
            // Scan accounting covers every stop path: a file with a reported match counts.
            if emitter.matched {
                c.files_matched += 1;
            }
            return Ok(Some(reason));
        }
        emitter.remember(line_no, line, a.context_lines);
        start = next;
        if line_no.is_multiple_of(4096) {
            st.check()?;
        }
    }
    if emitter.matched {
        c.files_matched += 1;
    }
    Ok(None)
}

struct Emitter<'t> {
    text: &'t mut TextBuilder,
    header: String,
    header_pushed: bool,
    last_emitted: Option<u64>,
    /// Previous lines, already rendered, for leading context (at most `context_lines`).
    prev: VecDeque<(u64, String)>,
    after_remaining: usize,
    matched: bool,
}

impl Emitter<'_> {
    fn render(line: &[u8]) -> String {
        let decoded = String::from_utf8_lossy(line);
        let (kept, removed) = cut_chars(&decoded, limits::SEARCH_LINE_CHARS);
        if removed > 0 {
            format!("{kept} [+{removed} chars]")
        } else {
            kept.to_owned()
        }
    }

    fn push(&mut self, line_no: u64, text: &str) -> bool {
        if !self.header_pushed {
            let header = std::mem::take(&mut self.header);
            if !self.text.try_push_line(&header) {
                return false;
            }
            self.header_pushed = true;
        }
        if let Some(last) = self.last_emitted
            && line_no > last + 1
            && !self.text.try_push_line("--")
        {
            return false;
        }
        if !self.text.try_push_line(text) {
            return false;
        }
        self.last_emitted = Some(line_no);
        true
    }

    fn emit_match(&mut self, line_no: u64, line: &[u8]) -> bool {
        let pending: Vec<(u64, String)> = self.prev.drain(..).collect();
        for (n, rendered) in pending {
            if self.last_emitted.is_none_or(|l| n > l) && !self.push(n, &format!("{n}-{rendered}"))
            {
                return false;
            }
        }
        let rendered = format!("{line_no}:{}", Self::render(line));
        if !self.push(line_no, &rendered) {
            return false;
        }
        self.matched = true;
        true
    }

    fn emit_context(&mut self, line_no: u64, line: &[u8]) -> bool {
        let rendered = format!("{line_no}-{}", Self::render(line));
        self.push(line_no, &rendered)
    }

    fn remember(&mut self, line_no: u64, line: &[u8], context: usize) {
        if context == 0 {
            return;
        }
        self.prev.push_back((line_no, Self::render(line)));
        if self.prev.len() > context {
            self.prev.pop_front();
        }
    }
}

/// Metadata with every integer at its widest and every enumeration at its longest value.
pub fn max_metadata() -> Value {
    let m = u64::MAX;
    json!({
        "query_chars": m,
        "matches": m,
        "files_scanned": m,
        "files_matched": m,
        "files_skipped": {
            "binary": m,
            "too_large": m,
            "denied": m,
            "symlink": m,
            "hidden": m,
            "excluded": m,
            "unreadable": m,
            "other": m,
            "too_deep": m,
        },
        "dirs_visited": m,
        "dirs_truncated": m,
        "bytes_scanned": m,
        "truncated": true,
        "stop_reason": "scan_budget",
    })
}
