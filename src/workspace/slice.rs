//! `file_slice`: a bounded range of lines from one regular file, streamed in 64 KiB steps.

use std::os::fd::BorrowedFd;

use serde_json::{Value, json};

use super::SliceArgs;
use super::confine::{Fail, open_regular, walk_dir};
use super::policy::{ERR_DENIED_PATH, ERR_NOT_READABLE, is_denied};
use super::render::{TextBuilder, cut_bytes, cut_chars, text_budget};
use super::worker::{Buffers, Built, LINE_STORE_BYTES, Stepper, Stop, metadata_max};
use crate::diag::JobKind;
use crate::limits;

struct Scan<'a> {
    a: &'a SliceArgs,
    file_bytes: u64,
    line_no: u64,
    returned: usize,
    text_bytes: usize,
    lines_cut: u64,
    end_line: Option<u64>,
    extra_chars: usize,
    extra_bytes: usize,
    last_byte: Option<u8>,
    has_bytes: bool,
    saw_lf: bool,
    saw_crlf: bool,
    lossy: bool,
    binary_checked: bool,
    binary: bool,
    pos: u64,
    bytes_read: u64,
    newline_count: u64,
    /// Newlines in bytes read but not processed after an early stop.
    tail_newlines: u64,
    /// The last byte of the file seen so far, for the trailing-segment rule.
    file_last_byte: Option<u8>,
    stop: Option<&'static str>,
    at_eof: bool,
    last_line_partial: bool,
    total_lines: Option<u64>,
}

impl<'a> Scan<'a> {
    fn new(a: &'a SliceArgs, file_bytes: u64) -> Scan<'a> {
        Scan {
            a,
            file_bytes,
            line_no: 1,
            returned: 0,
            text_bytes: 0,
            lines_cut: 0,
            end_line: None,
            extra_chars: 0,
            extra_bytes: 0,
            last_byte: None,
            has_bytes: false,
            saw_lf: false,
            saw_crlf: false,
            lossy: false,
            binary_checked: false,
            binary: false,
            pos: 0,
            bytes_read: 0,
            newline_count: 0,
            tail_newlines: 0,
            file_last_byte: None,
            stop: None,
            at_eof: false,
            last_line_partial: false,
            total_lines: None,
        }
    }

    fn collecting(&self) -> bool {
        self.line_no >= self.a.start_line
    }

    fn reset_line(&mut self, store: &mut Vec<u8>) {
        store.clear();
        self.extra_chars = 0;
        self.extra_bytes = 0;
        self.last_byte = None;
        self.has_bytes = false;
    }

    /// Processes a chunk. Returns the number of bytes consumed when collection stopped inside it.
    fn feed(&mut self, chunk: &[u8], text: &mut TextBuilder, store: &mut Vec<u8>) -> Option<usize> {
        if !self.binary_checked {
            self.binary_checked = true;
            let n = chunk.len().min(limits::BINARY_SNIFF_BYTES);
            if chunk[..n].contains(&0) {
                self.binary = true;
                self.stop = Some("binary");
                return Some(0);
            }
        }
        for (k, &byte) in chunk.iter().enumerate() {
            self.pos += 1;
            self.file_last_byte = Some(byte);
            if byte == b'\n' {
                self.newline_count += 1;
                let crlf = self.last_byte == Some(b'\r');
                if crlf {
                    self.saw_crlf = true;
                } else {
                    self.saw_lf = true;
                }
                if self.collecting() {
                    self.emit_line(true, crlf, text, store);
                }
                self.line_no += 1;
                self.reset_line(store);
                if self.stop.is_some() {
                    return Some(k + 1);
                }
            } else {
                self.has_bytes = true;
                self.last_byte = Some(byte);
                if self.collecting() {
                    if store.len() < LINE_STORE_BYTES {
                        store.push(byte);
                    } else {
                        self.extra_bytes += 1;
                        if byte & 0xC0 != 0x80 {
                            self.extra_chars += 1;
                        }
                        if self.extra_bytes > limits::LINE_LOOKAHEAD_BYTES {
                            self.emit_line(false, false, text, store);
                            if self.stop.is_none() {
                                self.stop = Some("line_too_long");
                            }
                            return Some(k + 1);
                        }
                    }
                }
            }
        }
        None
    }

    /// Accounts for the unprocessed remainder of a chunk after an early stop.
    fn tail(&mut self, rest: &[u8]) {
        self.tail_newlines += rest.iter().filter(|&&c| c == b'\n').count() as u64;
        if let Some(last) = rest.last() {
            self.file_last_byte = Some(*last);
        }
    }

    /// Emits the current line. `complete` is false when its end was not reached.
    fn emit_line(&mut self, complete: bool, strip_cr: bool, text: &mut TextBuilder, store: &[u8]) {
        let mut stored = store;
        let mut extra_chars = self.extra_chars;
        if strip_cr {
            if self.extra_bytes == 0 {
                if let Some(s) = stored.strip_suffix(b"\r") {
                    stored = s;
                }
            } else {
                extra_chars = extra_chars.saturating_sub(1);
            }
        }
        // When the store overflowed, a multibyte character may be split at the storage boundary:
        // its lead byte is stored and its continuation bytes were counted in the overflow (where
        // they are not characters). Decode up to the last complete character and count the split
        // one in the remainder; that is not lossy content.
        let decoded: std::borrow::Cow<'_, str> = match std::str::from_utf8(stored) {
            Ok(s) => std::borrow::Cow::Borrowed(s),
            Err(e) if e.error_len().is_none() && self.extra_bytes > 0 => {
                extra_chars += 1;
                std::borrow::Cow::Borrowed(
                    std::str::from_utf8(&stored[..e.valid_up_to()]).unwrap_or(""),
                )
            }
            Err(_) => {
                self.lossy = true;
                String::from_utf8_lossy(stored)
            }
        };
        let (kept, removed) = cut_chars(&decoded, limits::SLICE_LINE_CHARS);
        let mut removed_total = removed + extra_chars;
        let kept = if self.returned == 0 {
            let (k, r) = cut_bytes(kept, self.a.max_bytes);
            removed_total += r;
            k
        } else if self.text_bytes + kept.len() > self.a.max_bytes {
            self.stop = Some("max_bytes");
            return;
        } else {
            kept
        };
        let suffix = if !complete {
            " [+? chars]".to_owned()
        } else if removed_total > 0 {
            format!(" [+{removed_total} chars]")
        } else {
            String::new()
        };
        let line = format!("{}\t{kept}{suffix}", self.line_no);
        if !text.try_push_line(&line) {
            self.stop = Some("result_cap");
            return;
        }
        // Accounting follows admission: a line that was not admitted is not a returned cut line.
        if !suffix.is_empty() {
            self.lines_cut += 1;
        }
        self.returned += 1;
        self.text_bytes += kept.len();
        self.end_line = Some(self.line_no);
        if !complete {
            self.last_line_partial = true;
        }
        if self.returned >= self.a.max_lines {
            // `complete` when the file holds exactly these lines: every byte consumed.
            let all = self.pos == self.file_bytes && complete;
            self.at_eof = all;
            self.stop = Some(if all { "complete" } else { "max_lines" });
        }
    }

    fn eof(&mut self, text: &mut TextBuilder, store: &[u8]) {
        if self.stop.is_some() {
            return;
        }
        if self.has_bytes && self.collecting() {
            self.emit_line(true, false, text, store);
        }
        let final_lines = self.newline_count + u64::from(self.has_bytes);
        self.total_lines = Some(final_lines);
        if self.stop.is_none() {
            if self.returned == 0 {
                self.stop = Some("start_past_eof");
                self.at_eof = true;
            } else {
                self.stop = Some("complete");
                self.at_eof = !self.last_line_partial;
            }
        } else if self.stop == Some("max_lines") || self.stop == Some("complete") {
            self.at_eof = !self.last_line_partial;
            if self.stop == Some("max_lines") {
                self.stop = Some("complete");
            }
        }
    }

    fn deadline(&mut self, text: &mut TextBuilder, store: &[u8]) {
        if self.stop.is_some() {
            return;
        }
        if self.collecting() && self.has_bytes {
            self.emit_line(false, false, text, store);
        }
        if self.stop.is_none() {
            self.stop = Some("time_budget");
        }
    }
}

pub fn run(
    a: &SliceArgs,
    st: &mut Stepper,
    root: BorrowedFd<'_>,
    b: &mut Buffers,
) -> Result<Built, Stop> {
    b.text.reset(text_budget(metadata_max(JobKind::Slice)));
    let comps: Vec<&[u8]> = a.comps.iter().map(|s| s.as_bytes()).collect();
    let Some((name, parents)) = comps.split_last() else {
        return Ok(Built::Error(super::policy::ERR_INVALID_PATH));
    };
    if is_denied(name) {
        return Ok(Built::Error(ERR_DENIED_PATH));
    }
    let mut sc = Scan::new(a, 0);
    let dir = match walk_dir(st, root, parents) {
        Ok(d) => d,
        Err(Fail::Tool(m)) => return Ok(Built::Error(m)),
        Err(Fail::Stop(Stop::Deadline)) => return Ok(finish(&mut sc, b)),
        Err(Fail::Stop(s)) => return Err(s),
    };
    let (file, file_bytes) = match open_regular(st, dir.fd(), name) {
        Ok(x) => x,
        Err(Fail::Tool(m)) => return Ok(Built::Error(m)),
        Err(Fail::Stop(Stop::Deadline)) => return Ok(finish(&mut sc, b)),
        Err(Fail::Stop(s)) => return Err(s),
    };
    // Directory descriptors are dropped before any read; only the file descriptor remains.
    drop(dir);
    sc.file_bytes = file_bytes;
    b.line.clear();
    loop {
        match st.step(|fs| fs.read(&file, &mut b.read)) {
            Err(Stop::Deadline) => {
                sc.deadline(&mut b.text, &b.line);
                break;
            }
            Err(s) => return Err(s),
            Ok(Err(_)) => {
                if sc.returned == 0 {
                    return Ok(Built::Error(ERR_NOT_READABLE));
                }
                sc.deadline(&mut b.text, &b.line);
                break;
            }
            Ok(Ok(0)) => {
                sc.eof(&mut b.text, &b.line);
                break;
            }
            Ok(Ok(n)) => {
                sc.bytes_read += n as u64;
                if let Some(consumed) = sc.feed(&b.read[..n], &mut b.text, &mut b.line) {
                    sc.tail(&b.read[consumed..n]);
                    break;
                }
            }
        }
    }
    // Count the remaining lines of small files so `total_lines` can be reported.
    if sc.total_lines.is_none()
        && !sc.binary
        && sc.file_bytes <= limits::TOTAL_LINES_MAX_FILE_BYTES
        && sc.stop != Some("line_too_long")
    {
        let mut newlines = sc.newline_count + sc.tail_newlines;
        let mut last = sc.file_last_byte;
        let mut known = true;
        loop {
            match st.step(|fs| fs.read(&file, &mut b.read)) {
                Err(Stop::Deadline) | Ok(Err(_)) => {
                    known = false;
                    break;
                }
                Err(s) => return Err(s),
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => {
                    sc.bytes_read += n as u64;
                    let chunk = &b.read[..n];
                    newlines += chunk.iter().filter(|&&c| c == b'\n').count() as u64;
                    last = chunk.last().copied();
                }
            }
        }
        if known {
            let trailing = match last {
                Some(b'\n') | None => 0,
                Some(_) => 1,
            };
            sc.total_lines = Some(newlines + trailing);
        }
    }
    drop(file);
    Ok(finish(&mut sc, b))
}

fn finish(sc: &mut Scan<'_>, b: &mut Buffers) -> Built {
    let stop_reason = sc.stop.unwrap_or("time_budget");
    let truncated = stop_reason != "complete" || sc.last_line_partial;
    let text = if sc.binary {
        format!("[binary file, {} bytes]", sc.file_bytes)
    } else if sc.returned == 0 && stop_reason == "start_past_eof" {
        format!("[no lines: start_line {} is past EOF]", sc.a.start_line)
    } else if sc.returned == 0 && stop_reason == "time_budget" {
        format!(
            "[no lines: time budget exhausted before line {}]",
            sc.a.start_line
        )
    } else {
        b.text
            .finish(if truncated { Some(stop_reason) } else { None })
    };
    let line_endings = match (sc.saw_lf, sc.saw_crlf) {
        (false, false) => "none",
        (true, false) => "lf",
        (false, true) => "crlf",
        (true, true) => "mixed",
    };
    let mut meta = json!({
        "start_line": sc.a.start_line,
        "returned_lines": sc.returned,
        "at_eof": sc.at_eof,
        "last_line_partial": sc.last_line_partial,
        "file_bytes": sc.file_bytes,
        "bytes_read": sc.bytes_read,
        "encoding": if sc.binary { "binary" } else { "utf-8" },
        "lossy": sc.lossy,
        "line_endings": line_endings,
        "lines_cut": sc.lines_cut,
        "truncated": truncated,
        "stop_reason": stop_reason,
    });
    if let Some(end) = sc.end_line {
        meta["end_line"] = json!(end);
    }
    if let Some(total) = sc.total_lines {
        meta["total_lines"] = json!(total);
    }
    Built::Result {
        text,
        meta,
        stop_reason,
    }
}

/// Metadata with every integer at its widest and every enumeration at its longest value.
pub fn max_metadata() -> Value {
    let m = u64::MAX;
    json!({
        "start_line": m,
        "returned_lines": m,
        "end_line": m,
        "at_eof": false,
        "last_line_partial": false,
        "file_bytes": m,
        "bytes_read": m,
        "encoding": "binary",
        "lossy": false,
        "line_endings": "mixed",
        "lines_cut": m,
        "total_lines": m,
        "truncated": false,
        "stop_reason": "start_past_eof",
    })
}
