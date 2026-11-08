//! The Workspace Reading Pack: `workspace_outline`, `file_slice`, `text_search`, served over one
//! operator-declared root (`--root`) by an owned worker pool.
//!
//! Schemas are hand-written constants and are the contract (docs/tools.md). Argument validation
//! mirrors them exactly and yields fixed tool-error texts. Results are text only: the content
//! lines, an optional `[truncated: ...]` trailer, and a final `# key=value` metadata line. No
//! `structuredContent` and no advertised output schema: Claude Code 2.1.319 shows the model the
//! structured content instead of the text when both are present, which would hide every result.
//! The metadata schemas below describe the parsed metadata line and are used by tests and
//! documentation.

pub mod confine;
pub mod outline;
pub mod policy;
pub mod render;
pub mod search;
pub mod slice;
pub mod worker;

use std::io;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use rmcp::model::{CallToolResult, JsonObject, Tool, ToolAnnotations};
use serde_json::{Value, json};

use crate::diag::{Diag, JobKind};
use crate::limits;
use crate::supervisor::Shared;
use confine::{FileSystem, RealFs};
use policy::*;
use worker::{Pool, Snapshot, tool_error};

pub const OUTLINE: &str = "workspace_outline";
pub const SLICE: &str = "file_slice";
pub const SEARCH: &str = "text_search";
pub const TOOL_NAMES: [&str; 3] = [OUTLINE, SLICE, SEARCH];

pub const OUTLINE_DESCRIPTION: &str = "Lists the directory tree below the project root (or a subdirectory) as compact lines: `d path` for a directory, `f path bytes` for a file, `l path` for a symlink (never followed), `o path` for a special file. The last line, starting with `#`, is metadata (entries_listed, stop_reason, counters). Use it first to learn the layout before reading files. depth 1 to 3 (default 2), max_entries 1 to 200 (default 100). Hidden entries are skipped unless include_hidden is true. The directories .git, target, node_modules, .venv, and __pycache__ are listed but not entered. Read-only.";
pub const SLICE_DESCRIPTION: &str = "Returns lines of one text file below the project root as `<line>\\t<text>`, starting at start_line (default 1), at most max_lines lines (1 to 400, default 120) and max_bytes bytes of text (256 to 12288, default 8192). Lines over 400 characters are cut with a ` [+N chars]` suffix. A binary file returns a one-line marker. The last line, starting with `#`, is metadata (returned_lines, end_line, at_eof, total_lines, stop_reason). Use it instead of printing whole files; call again with a later start_line for more. Read-only.";
pub const SEARCH_DESCRIPTION: &str = "Searches text files below the project root (or a subdirectory or one file) for a literal string, 1 to 128 characters, not a regular expression. Output is grep-style: a `== path` header per file, `<line>:<text>` for matches, `<line>-<text>` for context lines (context_lines 0 to 2), `--` between separated groups, and a final `#` metadata line (matches, files_scanned, stop_reason). Case-insensitive by default (ASCII folding). Skips hidden entries unless include_hidden, binary files, files over 1 MiB, symlinks, and the directories .git, target, node_modules, .venv, __pycache__. Returns at most max_matches matches (1 to 100, default 40). Read-only.";

#[derive(Clone, Debug)]
pub struct OutlineArgs {
    pub comps: Vec<String>,
    pub depth: usize,
    pub max_entries: usize,
    pub include_hidden: bool,
}

#[derive(Clone, Debug)]
pub struct SliceArgs {
    pub comps: Vec<String>,
    pub start_line: u64,
    pub max_lines: usize,
    pub max_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct SearchArgs {
    pub query: String,
    pub comps: Vec<String>,
    pub case_sensitive: bool,
    pub max_matches: usize,
    pub context_lines: usize,
    pub include_hidden: bool,
}

#[derive(Clone, Debug)]
pub enum Args {
    Outline(OutlineArgs),
    Slice(SliceArgs),
    Search(SearchArgs),
}

fn object(v: Value) -> Arc<JsonObject> {
    Arc::new(
        v.as_object()
            .cloned()
            .expect("schema constant is an object"),
    )
}

fn path_property(description: &str) -> Value {
    json!({
        "type": "string",
        "maxLength": limits::MAX_PATH_BYTES,
        "description": description
    })
}

pub fn outline_input_schema() -> Arc<JsonObject> {
    object(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": path_property("Root-relative directory path with `/` separators; empty or omitted means the root. No leading `/`, no `.` or `..` components."),
            "depth": { "type": "integer", "minimum": 1, "maximum": 3, "default": 2, "description": "How many directory levels to list." },
            "max_entries": { "type": "integer", "minimum": 1, "maximum": 200, "default": 100, "description": "Maximum entries listed." },
            "include_hidden": { "type": "boolean", "default": false, "description": "List entries whose name starts with `.`." }
        }
    }))
}

pub fn outline_meta_schema() -> Arc<JsonObject> {
    object(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["entries_listed", "truncated", "stop_reason", "dirs_visited", "dirs_truncated", "dirs_unreadable", "excluded_dirs", "denied_entries", "symlinks", "hidden_skipped", "other_skipped", "too_deep"],
        "properties": {
            "entries_listed": { "type": "integer", "minimum": 0, "maximum": 200 },
            "truncated": { "type": "boolean" },
            "stop_reason": { "enum": ["complete", "max_entries", "raw_entry_limit", "result_cap", "time_budget"] },
            "dirs_visited": { "type": "integer", "minimum": 0 },
            "dirs_truncated": { "type": "integer", "minimum": 0 },
            "dirs_unreadable": { "type": "integer", "minimum": 0 },
            "excluded_dirs": { "type": "integer", "minimum": 0 },
            "denied_entries": { "type": "integer", "minimum": 0 },
            "symlinks": { "type": "integer", "minimum": 0 },
            "hidden_skipped": { "type": "integer", "minimum": 0 },
            "other_skipped": { "type": "integer", "minimum": 0 },
            "too_deep": { "type": "integer", "minimum": 0 }
        }
    }))
}

pub fn slice_input_schema() -> Arc<JsonObject> {
    object(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["path"],
        "properties": {
            "path": { "type": "string", "minLength": 1, "maxLength": limits::MAX_PATH_BYTES, "description": "Root-relative file path with `/` separators. No leading `/`, no `.` or `..` components." },
            "start_line": { "type": "integer", "minimum": 1, "maximum": 10000000, "default": 1, "description": "First line to return, 1-based." },
            "max_lines": { "type": "integer", "minimum": 1, "maximum": 400, "default": 120 },
            "max_bytes": { "type": "integer", "minimum": 256, "maximum": 12288, "default": 8192, "description": "Maximum bytes of returned line text." }
        }
    }))
}

pub fn slice_meta_schema() -> Arc<JsonObject> {
    object(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["start_line", "returned_lines", "at_eof", "last_line_partial", "file_bytes", "bytes_read", "encoding", "lossy", "line_endings", "lines_cut", "truncated", "stop_reason"],
        "properties": {
            "start_line": { "type": "integer", "minimum": 1 },
            "returned_lines": { "type": "integer", "minimum": 0, "maximum": 400 },
            "end_line": { "type": "integer", "minimum": 1 },
            "at_eof": { "type": "boolean" },
            "last_line_partial": { "type": "boolean" },
            "file_bytes": { "type": "integer", "minimum": 0 },
            "bytes_read": { "type": "integer", "minimum": 0 },
            "encoding": { "enum": ["utf-8", "binary"] },
            "lossy": { "type": "boolean" },
            "line_endings": { "enum": ["lf", "crlf", "mixed", "none"] },
            "lines_cut": { "type": "integer", "minimum": 0 },
            "total_lines": { "type": "integer", "minimum": 0 },
            "truncated": { "type": "boolean" },
            "stop_reason": { "enum": ["complete", "max_lines", "max_bytes", "result_cap", "time_budget", "line_too_long", "binary", "start_past_eof"] }
        }
    }))
}

pub fn search_input_schema() -> Arc<JsonObject> {
    object(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["query"],
        "properties": {
            "query": { "type": "string", "minLength": 1, "maxLength": 128, "description": "Literal text to find; not a regular expression; no control characters." },
            "path": path_property("Root-relative directory or file path; empty or omitted means the root."),
            "case_sensitive": { "type": "boolean", "default": false },
            "max_matches": { "type": "integer", "minimum": 1, "maximum": 100, "default": 40 },
            "context_lines": { "type": "integer", "minimum": 0, "maximum": 2, "default": 0 },
            "include_hidden": { "type": "boolean", "default": false }
        }
    }))
}

pub fn search_meta_schema() -> Arc<JsonObject> {
    object(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["query_chars", "matches", "files_scanned", "files_matched", "files_skipped", "dirs_visited", "dirs_truncated", "bytes_scanned", "truncated", "stop_reason"],
        "properties": {
            "query_chars": { "type": "integer", "minimum": 1, "maximum": 128 },
            "matches": { "type": "integer", "minimum": 0, "maximum": 100 },
            "files_scanned": { "type": "integer", "minimum": 0 },
            "files_matched": { "type": "integer", "minimum": 0 },
            "files_skipped": {
                "type": "object",
                "additionalProperties": false,
                "required": ["binary", "too_large", "denied", "symlink", "hidden", "excluded", "unreadable", "other", "too_deep"],
                "properties": {
                    "binary": { "type": "integer", "minimum": 0 },
                    "too_large": { "type": "integer", "minimum": 0 },
                    "denied": { "type": "integer", "minimum": 0 },
                    "symlink": { "type": "integer", "minimum": 0 },
                    "hidden": { "type": "integer", "minimum": 0 },
                    "excluded": { "type": "integer", "minimum": 0 },
                    "unreadable": { "type": "integer", "minimum": 0 },
                    "other": { "type": "integer", "minimum": 0 },
                    "too_deep": { "type": "integer", "minimum": 0 }
                }
            },
            "dirs_visited": { "type": "integer", "minimum": 0 },
            "dirs_truncated": { "type": "integer", "minimum": 0 },
            "bytes_scanned": { "type": "integer", "minimum": 0 },
            "truncated": { "type": "boolean" },
            "stop_reason": { "enum": ["complete", "max_matches", "max_files", "max_dirs", "max_entries", "result_cap", "scan_budget", "time_budget"] }
        }
    }))
}

fn annotations() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(true)
        .destructive(false)
        .idempotent(true)
        .open_world(false)
}

/// The three tool definitions, in the order they are listed.
pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new(OUTLINE, OUTLINE_DESCRIPTION, outline_input_schema())
            .with_title("Workspace outline")
            .with_annotations(annotations()),
        Tool::new(SLICE, SLICE_DESCRIPTION, slice_input_schema())
            .with_title("File slice")
            .with_annotations(annotations()),
        Tool::new(SEARCH, SEARCH_DESCRIPTION, search_input_schema())
            .with_title("Text search")
            .with_annotations(annotations()),
    ]
}

pub fn is_tool(name: &str) -> bool {
    TOOL_NAMES.contains(&name)
}

fn get_int(
    args: &JsonObject,
    key: &str,
    min: u64,
    max: u64,
    default: u64,
) -> Result<u64, &'static str> {
    match args.get(key) {
        None => Ok(default),
        Some(Value::Number(n)) => match n.as_u64() {
            Some(v) if v >= min && v <= max => Ok(v),
            _ => Err(ERR_INVALID_ARGUMENT),
        },
        Some(_) => Err(ERR_INVALID_ARGUMENT),
    }
}

fn get_bool(args: &JsonObject, key: &str, default: bool) -> Result<bool, &'static str> {
    match args.get(key) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(ERR_INVALID_ARGUMENT),
    }
}

/// An optional path argument (outline, search): absent or `""` means the root.
fn get_dir_path(args: &JsonObject) -> Result<Vec<String>, &'static str> {
    match args.get("path") {
        None => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(validate_path(s)?.into_iter().map(str::to_owned).collect()),
        Some(_) => Err(ERR_INVALID_PATH),
    }
}

fn check_keys(args: &JsonObject, allowed: &[&str]) -> Result<(), &'static str> {
    if args.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(ERR_UNEXPECTED_ARGUMENT);
    }
    Ok(())
}

fn check_size(args: &JsonObject) -> Result<(), &'static str> {
    let serialized = serde_json::to_vec(args)
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    if serialized > limits::MAX_ARGUMENTS_BYTES {
        return Err(ERR_ARGUMENTS_TOO_LARGE);
    }
    Ok(())
}

/// Validates arguments against the frozen contracts. Errors are fixed tool-error texts.
pub fn parse_args(
    name: &str,
    arguments: Option<&JsonObject>,
) -> Result<(JobKind, Args), &'static str> {
    let empty = JsonObject::new();
    let args = arguments.unwrap_or(&empty);
    check_size(args)?;
    match name {
        OUTLINE => {
            check_keys(args, &["path", "depth", "max_entries", "include_hidden"])?;
            let comps = get_dir_path(args)?;
            let depth = get_int(args, "depth", 1, 3, 2)? as usize;
            let max_entries = get_int(args, "max_entries", 1, 200, 100)? as usize;
            let include_hidden = get_bool(args, "include_hidden", false)?;
            Ok((
                JobKind::Outline,
                Args::Outline(OutlineArgs {
                    comps,
                    depth,
                    max_entries,
                    include_hidden,
                }),
            ))
        }
        SLICE => {
            check_keys(args, &["path", "start_line", "max_lines", "max_bytes"])?;
            let comps = match args.get("path") {
                None => return Err(ERR_MISSING_PATH),
                Some(Value::String(s)) => {
                    let comps = validate_path(s)?;
                    if comps.is_empty() {
                        return Err(ERR_INVALID_PATH);
                    }
                    comps.into_iter().map(str::to_owned).collect()
                }
                Some(_) => return Err(ERR_INVALID_PATH),
            };
            let start_line = get_int(args, "start_line", 1, 10_000_000, 1)?;
            let max_lines = get_int(args, "max_lines", 1, 400, 120)? as usize;
            let max_bytes = get_int(
                args,
                "max_bytes",
                256,
                limits::MAX_WORKSPACE_RESULT_BYTES as u64,
                8_192,
            )? as usize;
            Ok((
                JobKind::Slice,
                Args::Slice(SliceArgs {
                    comps,
                    start_line,
                    max_lines,
                    max_bytes,
                }),
            ))
        }
        SEARCH => {
            check_keys(
                args,
                &[
                    "query",
                    "path",
                    "case_sensitive",
                    "max_matches",
                    "context_lines",
                    "include_hidden",
                ],
            )?;
            let query = match args.get("query") {
                Some(Value::String(q)) => {
                    let chars = q.chars().count();
                    if chars == 0
                        || chars > 128
                        || q.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}')
                    {
                        return Err(ERR_INVALID_ARGUMENT);
                    }
                    q.clone()
                }
                _ => return Err(ERR_INVALID_ARGUMENT),
            };
            let comps = get_dir_path(args)?;
            let case_sensitive = get_bool(args, "case_sensitive", false)?;
            let max_matches = get_int(args, "max_matches", 1, 100, 40)? as usize;
            let context_lines = get_int(args, "context_lines", 0, 2, 0)? as usize;
            let include_hidden = get_bool(args, "include_hidden", false)?;
            Ok((
                JobKind::Search,
                Args::Search(SearchArgs {
                    query,
                    comps,
                    case_sensitive,
                    max_matches,
                    context_lines,
                    include_hidden,
                }),
            ))
        }
        _ => Err(ERR_INVALID_ARGUMENT),
    }
}

/// The pack: a root descriptor and its worker pool.
pub struct Workspace {
    pool: Arc<Pool>,
}

impl Workspace {
    /// Opens `root` once and starts the workers. The only use of ambient path authority.
    pub fn open(root: &str, shared: Arc<Shared>, diag: Diag) -> io::Result<Workspace> {
        let fd = confine::open_root(root)?;
        Ok(Workspace::with_fs(Arc::new(RealFs), fd, shared, diag))
    }

    /// Starts the workers over an already-open root with the given system-call implementation.
    pub fn with_fs(
        fs: Arc<dyn FileSystem>,
        root: OwnedFd,
        shared: Arc<Shared>,
        diag: Diag,
    ) -> Workspace {
        Workspace {
            pool: Pool::start(fs, root, shared, diag),
        }
    }

    pub fn jobs_counter(&self) -> Arc<AtomicUsize> {
        self.pool.jobs_counter()
    }

    pub fn snapshot(&self) -> Snapshot {
        self.pool.snapshot()
    }

    /// Validates arguments and runs the tool under the job slot and deadline rules. Every failure
    /// is a fixed tool error; protocol errors are the handler's (deadline, cancellation).
    pub async fn call(
        &self,
        name: &str,
        arguments: Option<&JsonObject>,
        deadline: Instant,
    ) -> CallToolResult {
        let (kind, args) = match parse_args(name, arguments) {
            Ok(x) => x,
            Err(message) => return tool_error(message),
        };
        match self.pool.execute(kind, args, deadline).await {
            Ok(result) => result,
            Err(message) => tool_error(message),
        }
    }
}
