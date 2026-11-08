//! Text rendering with exact escaped-length accounting.
//!
//! A result is built, never trimmed: every text unit is admitted only if its JSON-escaped length
//! fits the remaining budget, so no over-budget candidate ever exists (TS-M1-REVIEW-003 F03).

use std::io::Write;

use crate::limits;

/// Upper bound on the fixed JSON scaffold around text and metadata; checked by a test.
pub const SCAFFOLD_BYTES: usize = 128;
/// Upper bound on the serialized trailer line; checked by a test.
pub const TRAILER_MAX: usize = 48;

/// Exact length of `s` once JSON-escaped by serde_json: `"` and `\` two bytes; U+0008, U+0009,
/// U+000A, U+000C, U+000D two bytes; other code points below U+0020 six bytes; everything else
/// (including U+007F and non-ASCII) its UTF-8 length.
pub fn escaped_len(s: &str) -> usize {
    let mut n = 0;
    for b in s.bytes() {
        n += match b {
            b'"' | b'\\' | 0x08 | 0x09 | 0x0a | 0x0c | 0x0d => 2,
            0x00..=0x1f => 6,
            _ => 1,
        };
    }
    n
}

/// Cuts `s` to at most `max_chars` characters at a character boundary. Returns the kept prefix
/// and the number of characters removed.
pub fn cut_chars(s: &str, max_chars: usize) -> (&str, usize) {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => (&s[..idx], s[idx..].chars().count()),
        None => (s, 0),
    }
}

/// Cuts `s` to at most `max_bytes` UTF-8 bytes at a character boundary. Returns the kept prefix
/// and the number of characters removed.
pub fn cut_bytes(s: &str, max_bytes: usize) -> (&str, usize) {
    if s.len() <= max_bytes {
        return (s, 0);
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (&s[..end], s[end..].chars().count())
}

/// Builds the text block under a byte budget expressed in escaped bytes.
pub struct TextBuilder {
    text: String,
    escaped: usize,
    budget: usize,
    lines: usize,
}

impl TextBuilder {
    pub fn with_capacity(capacity: usize) -> TextBuilder {
        TextBuilder {
            text: String::with_capacity(capacity),
            escaped: 0,
            budget: 0,
            lines: 0,
        }
    }

    /// Clears the text and sets the budget for the next result. Capacity is kept.
    pub fn reset(&mut self, budget: usize) {
        self.text.clear();
        self.escaped = 0;
        self.budget = budget;
        self.lines = 0;
    }

    pub fn capacity(&self) -> usize {
        self.text.capacity()
    }

    pub fn lines(&self) -> usize {
        self.lines
    }

    pub fn escaped(&self) -> usize {
        self.escaped
    }

    pub fn remaining(&self) -> usize {
        self.budget.saturating_sub(self.escaped)
    }

    /// Escaped cost of one unit: its escaped length plus the two-byte escaped newline.
    pub fn cost(line: &str) -> usize {
        escaped_len(line) + 2
    }

    /// Appends a line when it fits. Returns false, leaving the text unchanged, when it does not.
    pub fn try_push_line(&mut self, line: &str) -> bool {
        let cost = Self::cost(line);
        if self.escaped + cost > self.budget {
            return false;
        }
        if self.lines > 0 {
            self.text.push('\n');
        }
        self.text.push_str(line);
        self.escaped += cost;
        self.lines += 1;
        true
    }

    /// Returns a copy of the text, with `[truncated: <reason>]` appended as the last line when
    /// given. The builder keeps its storage and capacity for the next job.
    pub fn finish(&mut self, trailer: Option<&str>) -> String {
        if let Some(reason) = trailer {
            if self.lines > 0 {
                self.text.push('\n');
            }
            self.text.push_str("[truncated: ");
            self.text.push_str(reason);
            self.text.push(']');
        }
        self.text.clone()
    }
}

/// Text budget for a tool whose metadata line costs at most `meta_line_max` escaped bytes.
pub fn text_budget(meta_line_max: usize) -> usize {
    limits::MAX_WORKSPACE_RESULT_BYTES
        .saturating_sub(SCAFFOLD_BYTES)
        .saturating_sub(meta_line_max)
        .saturating_sub(TRAILER_MAX)
}

/// Renders the metadata object as the final text line: `# key=value ...`, nested objects flattened
/// with `.`, keys in the serializer's sorted order. Clients that prefer `structuredContent` over
/// the text block (Claude Code 2.1.319 does) would otherwise hide the content from the model, so
/// the workspace tools return text only and carry their metadata in this line.
pub fn meta_line(meta: &serde_json::Value) -> String {
    let mut out = String::from("#");
    flatten(meta, "", &mut out);
    out
}

fn flatten(v: &serde_json::Value, prefix: &str, out: &mut String) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, x) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(x, &key, out);
            }
        }
        other => {
            out.push(' ');
            out.push_str(prefix);
            out.push('=');
            match other {
                serde_json::Value::String(s) => out.push_str(s),
                _ => out.push_str(&other.to_string()),
            }
        }
    }
}

/// Parses a metadata line back into a JSON object (integers, booleans, strings; `a.b` keys
/// nested). Used by tests and the evidence tooling.
pub fn parse_meta_line(line: &str) -> Option<serde_json::Value> {
    let rest = line.strip_prefix('#')?;
    let mut root = serde_json::Map::new();
    for pair in rest.split(' ').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=')?;
        let value = if v == "true" {
            serde_json::Value::Bool(true)
        } else if v == "false" {
            serde_json::Value::Bool(false)
        } else if let Ok(n) = v.parse::<u64>() {
            serde_json::Value::Number(n.into())
        } else {
            serde_json::Value::String(v.to_owned())
        };
        let mut parts: Vec<&str> = k.split('.').collect();
        let last = parts.pop()?;
        let mut cur = &mut root;
        for p in parts {
            let entry = cur
                .entry(p.to_owned())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            cur = entry.as_object_mut()?;
        }
        cur.insert(last.to_owned(), value);
    }
    Some(serde_json::Value::Object(root))
}

/// A writer with a fixed capacity that fails instead of growing.
pub struct BoundedWriter<'a> {
    buf: &'a mut Vec<u8>,
    capacity: usize,
}

impl<'a> BoundedWriter<'a> {
    pub fn new(buf: &'a mut Vec<u8>) -> BoundedWriter<'a> {
        let capacity = buf.capacity();
        buf.clear();
        BoundedWriter { buf, capacity }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

impl Write for BoundedWriter<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.buf.len() + data.len() > self.capacity {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "result buffer capacity",
            ));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaped_len_matches_serde_json() {
        let samples: Vec<String> = vec![
            String::new(),
            "plain".into(),
            "quote\" back\\ tab\t nl\n cr\r bs\u{8} ff\u{c}".into(),
            "\u{1}\u{2}\u{1f}\u{7f}".into(),
            "été 日本 \u{1F600}".into(),
            "x".repeat(400) + &"\u{1}".repeat(199),
        ];
        for s in &samples {
            let json = serde_json::to_string(s).unwrap();
            assert_eq!(escaped_len(s) + 2, json.len(), "{s:?}");
        }
        // Every byte value as a one-byte string where valid, plus random sequences.
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..500 {
            let mut bytes = Vec::new();
            for _ in 0..32 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                bytes.push((seed & 0xff) as u8);
            }
            let s = String::from_utf8_lossy(&bytes).into_owned();
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(escaped_len(&s) + 2, json.len(), "{s:?}");
        }
    }

    #[test]
    fn cutting_respects_boundaries() {
        assert_eq!(cut_chars("abc", 5), ("abc", 0));
        assert_eq!(cut_chars("abcdef", 3), ("abc", 3));
        assert_eq!(cut_chars("日本語テキスト", 2), ("日本", 5));
        assert_eq!(cut_bytes("abcdef", 3), ("abc", 3));
        assert_eq!(cut_bytes("日本語", 4), ("日", 2));
        assert_eq!(cut_bytes("日本語", 9), ("日本語", 0));
    }

    #[test]
    fn builder_admits_only_what_fits_and_never_grows() {
        let mut b = TextBuilder::with_capacity(limits::MAX_WORKSPACE_RESULT_BYTES);
        let cap = b.capacity();
        b.reset(20);
        assert!(b.try_push_line("12345678")); // 8 + 2
        assert!(b.try_push_line("12345678")); // 16 + 2 = 20
        assert!(!b.try_push_line("x")); // would be 23
        assert_eq!(b.lines(), 2);
        let text = b.finish(Some("result_cap"));
        assert_eq!(text, "12345678\n12345678\n[truncated: result_cap]");
        assert_eq!(b.capacity(), cap);
        b.reset(1_000);
        assert_eq!(b.lines(), 0);
        let text = b.finish(None);
        assert_eq!(text, "");
    }

    #[test]
    fn scaffold_and_trailer_bounds_hold() {
        use rmcp::model::{CallToolResult, ContentBlock};
        let r = CallToolResult::success(vec![ContentBlock::text("")]);
        let bytes = serde_json::to_vec(&r).unwrap();
        assert!(bytes.len() <= SCAFFOLD_BYTES, "{}", bytes.len());
        let meta = serde_json::json!({"a": 1, "b": {"c": true, "d": "x"}, "e": "y"});
        let line = meta_line(&meta);
        assert_eq!(line, "# a=1 b.c=true b.d=x e=y");
        assert_eq!(parse_meta_line(&line).unwrap(), meta);
        let longest = "[truncated: raw_entry_limit]";
        assert!(TextBuilder::cost(longest) <= TRAILER_MAX);
        for reason in [
            "complete",
            "max_entries",
            "raw_entry_limit",
            "result_cap",
            "time_budget",
            "max_lines",
            "max_bytes",
            "line_too_long",
            "binary",
            "start_past_eof",
            "max_matches",
            "max_files",
            "max_dirs",
            "scan_budget",
        ] {
            assert!(TextBuilder::cost(&format!("[truncated: {reason}]")) <= TRAILER_MAX);
        }
    }

    #[test]
    fn bounded_writer_fails_instead_of_growing() {
        let mut buf: Vec<u8> = Vec::with_capacity(8);
        let mut w = BoundedWriter::new(&mut buf);
        assert!(w.write_all(b"12345678").is_ok());
        assert!(w.write_all(b"9").is_err());
        assert_eq!(w.len(), 8);
        assert_eq!(buf.capacity(), 8);
    }
}
