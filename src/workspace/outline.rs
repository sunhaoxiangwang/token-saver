//! `workspace_outline`: a bounded, sorted, breadth-first listing of a directory tree.

use std::os::fd::BorrowedFd;

use serde_json::{Value, json};

use super::OutlineArgs;
use super::confine::{Fail, Kind, classify, list_dir, walk_dir};
use super::policy::{is_denied, is_excluded_dir, is_hidden, render_name, within_path_limits};
use super::render::text_budget;
use super::worker::{Buffers, Built, Node, Stepper, Stop, metadata_max};
use crate::diag::JobKind;
use crate::limits;

#[derive(Default)]
struct Counters {
    dirs_visited: u64,
    dirs_truncated: u64,
    dirs_unreadable: u64,
    excluded_dirs: u64,
    denied_entries: u64,
    symlinks: u64,
    hidden_skipped: u64,
    other_skipped: u64,
    too_deep: u64,
    raw_total: usize,
}

/// Joins components with `/` into one path.
pub fn join(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        out.extend_from_slice(p);
    }
    out
}

/// Collects the names from the arena root to `idx` (excluding the start node).
pub fn arena_path(arena: &[Node], idx: usize) -> Vec<Vec<u8>> {
    let mut names = Vec::new();
    let mut k = idx;
    while arena[k].parent != u32::MAX {
        names.push(arena[k].name.clone());
        k = arena[k].parent as usize;
    }
    names.reverse();
    names
}

pub fn run(
    a: &OutlineArgs,
    st: &mut Stepper,
    root: BorrowedFd<'_>,
    b: &mut Buffers,
) -> Result<Built, Stop> {
    b.text.reset(text_budget(metadata_max(JobKind::Outline)));
    let mut c = Counters::default();
    let mut listed = 0usize;
    let base: Vec<&[u8]> = a.comps.iter().map(|s| s.as_bytes()).collect();
    b.arena.clear();
    b.arena.push(Node {
        parent: u32::MAX,
        depth: 0,
        name: Vec::new(),
    });
    let stop_reason = match collect(a, &base, st, root, b, &mut c, &mut listed) {
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
        "entries_listed": listed,
        "truncated": truncated,
        "stop_reason": stop_reason,
        "dirs_visited": c.dirs_visited,
        "dirs_truncated": c.dirs_truncated,
        "dirs_unreadable": c.dirs_unreadable,
        "excluded_dirs": c.excluded_dirs,
        "denied_entries": c.denied_entries,
        "symlinks": c.symlinks,
        "hidden_skipped": c.hidden_skipped,
        "other_skipped": c.other_skipped,
        "too_deep": c.too_deep,
    });
    Ok(Built::Result {
        text,
        meta,
        stop_reason,
    })
}

fn collect(
    a: &OutlineArgs,
    base: &[&[u8]],
    st: &mut Stepper,
    root: BorrowedFd<'_>,
    b: &mut Buffers,
    c: &mut Counters,
    listed: &mut usize,
) -> Result<&'static str, Fail> {
    let mut i = 0;
    while i < b.arena.len() {
        let node_depth = b.arena[i].depth as usize;
        let names = arena_path(&b.arena, i);
        let mut full: Vec<&[u8]> = base.to_vec();
        full.extend(names.iter().map(|n| n.as_slice()));
        let dir = match walk_dir(st, root, &full) {
            Ok(d) => d,
            Err(Fail::Tool(message)) => {
                if i == 0 {
                    return Err(Fail::Tool(message));
                }
                // Moved, removed, or unreadable since it was queued: re-resolution sees the
                // tree as it is now.
                c.dirs_unreadable += 1;
                i += 1;
                continue;
            }
            Err(Fail::Stop(s)) => return Err(Fail::Stop(s)),
        };
        let remaining_raw = limits::OUTLINE_RAW_ENTRY_LIMIT.saturating_sub(c.raw_total);
        if remaining_raw == 0 {
            return Ok("raw_entry_limit");
        }
        let limit = remaining_raw.min(limits::DIR_ENTRY_LIMIT);
        let truncated = list_dir(st, dir.fd(), limit, &mut b.listing)?;
        c.raw_total += b.listing.len();
        c.dirs_visited += 1;
        if truncated {
            c.dirs_truncated += 1;
        }
        b.listing.sort_by(|x, y| x.name.cmp(&y.name));
        let prefix = join(&full);
        let components = full.len() + 1;
        for e in b.listing.iter() {
            st.check()?;
            if is_hidden(&e.name) && !a.include_hidden {
                c.hidden_skipped += 1;
                continue;
            }
            let kind = if e.kind == Kind::Unknown {
                match classify(st, dir.fd(), &e.name) {
                    Ok(m) => m.kind,
                    Err(Fail::Tool(_)) => {
                        c.other_skipped += 1;
                        continue;
                    }
                    Err(Fail::Stop(s)) => return Err(Fail::Stop(s)),
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
            let rendered = render_name(&path);
            let line = match kind {
                Kind::Directory => {
                    if is_excluded_dir(&e.name) {
                        c.excluded_dirs += 1;
                        format!("d {rendered} excluded")
                    } else {
                        if node_depth + 1 < a.depth
                            && b.arena.len() < limits::OUTLINE_RAW_ENTRY_LIMIT
                        {
                            b.arena.push(Node {
                                parent: i as u32,
                                depth: (node_depth + 1) as u16,
                                name: e.name.clone(),
                            });
                        }
                        format!("d {rendered}")
                    }
                }
                Kind::Regular => {
                    if is_denied(&e.name) {
                        c.denied_entries += 1;
                        format!("f {rendered} denied")
                    } else {
                        match classify(st, dir.fd(), &e.name) {
                            Ok(m) => format!("f {rendered} {}", m.size),
                            Err(Fail::Tool(_)) => {
                                c.other_skipped += 1;
                                continue;
                            }
                            Err(Fail::Stop(s)) => return Err(Fail::Stop(s)),
                        }
                    }
                }
                Kind::Symlink => {
                    c.symlinks += 1;
                    format!("l {rendered}")
                }
                Kind::Other | Kind::Unknown => {
                    c.other_skipped += 1;
                    format!("o {rendered}")
                }
            };
            if !b.text.try_push_line(&line) {
                return Ok("result_cap");
            }
            *listed += 1;
            if *listed >= a.max_entries {
                return Ok("max_entries");
            }
        }
        if c.raw_total >= limits::OUTLINE_RAW_ENTRY_LIMIT && (truncated || i + 1 < b.arena.len()) {
            return Ok("raw_entry_limit");
        }
        i += 1;
    }
    Ok("complete")
}

/// Metadata with every integer at its widest and every enumeration at its longest value.
pub fn max_metadata() -> Value {
    let m = u64::MAX;
    json!({
        "entries_listed": m,
        "truncated": true,
        "stop_reason": "raw_entry_limit",
        "dirs_visited": m,
        "dirs_truncated": m,
        "dirs_unreadable": m,
        "excluded_dirs": m,
        "denied_entries": m,
        "symlinks": m,
        "hidden_skipped": m,
        "other_skipped": m,
        "too_deep": m,
    })
}
