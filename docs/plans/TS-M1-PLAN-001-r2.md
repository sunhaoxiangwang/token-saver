# TS-M1-PLAN-001, Milestone 1 plan, revision 2 (plan only)

Submitted 2026-11-06 against `main` at `013734d` (release binary `0dc00896…`). Nothing in this
document is implemented. Token savings are not claimed; the plan defines how they are measured.

Changes from revision 1: C01 closed in the M0 evidence (commit `013734d`); F01 replaces Tokio's
blocking pool with an owned, accounted worker pool supervised through completion; F02 replaces
canonicalize-then-open with descriptor-relative, no-follow, single-component resolution from a root
descriptor and names its dependency; F03 freezes every contract and resolves U3 to U5; F04 adds
attribution controls to the comparison; F05 adds digest correlation and relay changes to scope.

## 0. Inputs and the open requirement

Verified facts are unchanged from revision 1: no Milestone 1 specification exists; the M0 server
exposes one tool and claims no savings; Claude Code and Codex report usage per run in
machine-readable form, and in the recorded single-probe sessions the fixed context dominated
(Claude Code run 2: 62,354 cache-read tokens and 66 uncached input tokens for a 382-byte result;
Codex: 29,868 input tokens, 27,008 cached, for a 358-byte result); ChatGPT exposes no counts.
Through the tunnel, ChatGPT used the modern era and reused request ids across requests
(`docs/evidence/m0/chatgpt.relay.records.jsonl`).

The reviewer recommends workload (a), inspecting project code and documents, with separate
measurements for Claude Code and Codex and functional tests plus payload measurements for ChatGPT.
This revision follows that recommendation. The user's spending priority remains unconfirmed and is
carried as unresolved requirement U1; a different answer changes sections 2 to 9 and becomes
revision 3.

## 1. Workflow and measurable benefit

An assistant answering a question about a project fetches a directory outline, a bounded slice of a
file, or literal search hits, and answers from them. Today Claude Code and Codex do this through
native tools or a shell whose output enters the context bounded only by the client's truncation;
ChatGPT Web cannot do it without pasted content. The benefit is measured, per client, task, and
arm, as client-reported usage, model-visible tool-result bytes, tool-call counts, and correctness
(section 8). The claim form after measurement is "task T on client C: N fewer uncached input
tokens and M fewer output tokens, median of five compliant runs, correctness unchanged", never a
bare percentage.

## 2. Scope and alternatives

Proposed: a Workspace Reading Pack of three read-only tools over one operator-declared root, an
owned worker pool, descriptor-relative confinement, relay and harness extensions for correlation,
and a comparison harness. No writes, execution, network, persistent state, indexing, or symlink
following.

| Alternative | Benefit | Cost | Decision |
|---|---|---|---|
| (a) Workspace Reading Pack | bounded, compact file and directory views for all clients; measurable baseline | file-read authority under a root; secret exposure inside the root; new dependency for confinement | proposed, recommended by review |
| (b) Command-output digests | largest raw volume to compress | execution authority, per-tool parsing, allowlist governance | deferred until (a) is measured |
| (c) Host facts | fills a ChatGPT capability gap | little to compress; host identifiers leave the machine; not approved | not proposed |
| (d) Notes or context store | avoids re-sending content | persistent state, excluded by the M0 architecture | not proposed |
| (e) No new tools | no authority change | no token-saving functionality | not proposed |

## 3. Confinement (F02)

### 3.1 Mechanism

Root descriptor. At startup, before serving, `main` requires `--root <absolute path>`, opens it
once with `rustix::fs::open(path, RDONLY | DIRECTORY | CLOEXEC | NOFOLLOW)`, confirms a directory
with `fstat`, and hands the `OwnedFd` to the worker pool, which holds it for the process lifetime.
This is the only use of ambient path authority. The textual root never appears in results or
diagnostics.

Argument validation (lexical, in the handler, no I/O). A path argument is UTF-8, 1 to 1,024 bytes,
contains no NUL, does not start with `/`, splits on `/` into 1 to 64 components, each 1 to 255
bytes and neither `.` nor `..`. The empty string denotes the root where a directory is expected.
Anything else is `invalid path` before any descriptor is touched.

Resolution (in the worker, section 4). Starting from the root descriptor, each ancestor component is
opened with `rustix::fs::openat(current, component, RDONLY | DIRECTORY | CLOEXEC | NOFOLLOW)`; the
previous descriptor is closed once the next is open, so at most two component descriptors are live
per job. `ELOOP` (the component is a symlink), `ENOTDIR`, and `ENOENT` map to `invalid path`,
`not a directory`, and `not found`; `EACCES` maps to `not readable`. The final component is opened
with `openat(current, name, RDONLY | CLOEXEC | NOFOLLOW)`, plus `DIRECTORY` when a directory is
required, then `fstat` and `FileType::from_raw_mode(st_mode)` must report a regular file or a
directory as the tool requires; anything else is `not a file` or `not a directory`. All subsequent
I/O uses that descriptor: `std::fs::File::from(OwnedFd)` for reads (a safe conversion),
`rustix::fs::Dir::read_from(&fd)` for listing, and the same `openat` rule for every child
directory or file visited during traversal. A `DirEntry` whose `file_type()` is a symlink is
recorded as `symlink` and never opened; any other non-regular, non-directory type is `other` and
never opened. The kernel is never asked to resolve more than one component, and never to follow a
symlink.

### 3.2 Declared guarantee

Every descriptor the tools read from is obtained by a chain of descriptor-relative, no-follow,
single-component opens that begins at the root descriptor. Each object opened is, at the instant
of its open, a direct child of a directory already reached by that chain. Therefore no replacement
of any ancestor component, of the final component, or of the root path after startup, whether by a
symlink or by a directory, can direct an open outside the subtree of the directory the root
descriptor refers to; a component replaced by a symlink fails with `ELOOP`, and a component
replaced by another directory yields that directory, which sits inside the already-opened parent.

Documented residuals, not escapes: a directory or file moved into the root from outside becomes
readable because it is now inside; a mount point inside the root is inside; a hard link inside the
root to a file whose other names lie outside exposes that file's content, which the operator placed
there; if the root directory itself is renamed, the descriptor follows it and reads continue in the
same tree; if it is deleted, children resolve to `not found`.

### 3.3 Dependency and `forbid(unsafe_code)` implications

The standard library exposes no `openat`, so descriptor-relative resolution needs a system-call
crate. Proposed: `rustix = "=1.1.6"`, `default-features = false`, features `std`, `alloc`, `fs`
(repository bytecodealliance/rustix; license `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT`,
which `deny.toml` accepts through the `Apache-2.0` and `MIT` alternatives). Its `openat`, `fstat`,
`Dir`, and `DirEntry` are safe functions over `OwnedFd` and `AsFd`; rustix contains `unsafe` FFI
internally, like every system-call crate, and the token-saver crate keeps `#![forbid(unsafe_code)]`
unchanged. Transitive additions on macOS are `bitflags` and `libc` (already a dev-dependency), on
Linux `linux-raw-sys`; the exact set, the binary size delta, `cargo deny`, `cargo audit`, and the
third-party notices are reported with the implementation. The resolver itself is about 150 lines
owned by this project and tested per 3.4.

Alternative considered: `cap-std = "=4.0.3"` (same licensing), whose `Dir` performs sandboxed
resolution and follows symlinks that stay inside the sandbox. It is more widely exercised but
brings a larger dependency tree and a symlink policy this plan does not want. Decision D1 asks the
reviewer to confirm rustix with the owned resolver, or to require cap-std instead; the tests in 3.4
apply to either.

### 3.4 Verification

The worker's system calls sit behind a `FileSystem` trait with the rustix implementation in
production and a fixture wrapper in `tests/` that can pause between any two calls (the same pattern
as the M0 `GateService`). Deterministic tests, each asserting that no outside content is ever
returned and that the expected error or the original content appears:

1. pre-existing symlinked ancestor, symlinked final component, symlinked root argument;
2. pause after opening `a`, replace `a/b` with a symlink to an outside directory, resume: `invalid
   path`;
3. pause after opening `a/b`, replace `a` with a symlink to an outside directory, resume: the job
   reads the original `a/b/file` through the open descriptor;
4. pause after the directory open, swap the final file for a symlink, resume: `invalid path`;
5. rename the root after startup and create a new directory with a sentinel file at the old path:
   tools keep reading the original tree, the sentinel is unreachable;
6. lexical rejections: `..`, `.`, empty component, leading `/`, NUL, 65 components, a 256-byte
   component, a 1,025-byte path;
7. a concurrent flipper thread alternating a directory between real and symlink-to-outside for
   two seconds under continuous reads: zero outside content, every result either original content
   or `invalid path`.

## 4. Blocking work: ownership, accounting, supervision (F01)

### 4.1 Why not `spawn_blocking`

Tokio documents that tasks spawned with `spawn_blocking` cannot be aborted, that a started closure
keeps running after the awaiting task is dropped, and that runtime shutdown waits indefinitely for
started blocking tasks unless `shutdown_timeout` abandons them, in which case they keep running. A
handler that returns `-32603` at its deadline would leave its read running, retire its request, and
free an admission permit for the next request to start another; the thread limit is "very large by
default". Nothing in that path is owned by this server. The plan therefore uses no `spawn_blocking`.

### 4.2 Worker pool

- Four worker threads created at startup when `--root` is given, owned by `workspace::worker`,
  each holding a borrowed root descriptor. Without `--root` no thread exists.
- Eight job slots, a `tokio::sync::Semaphore(8)`. The handler acquires a slot before enqueueing;
  the permit moves into the job and is released by the worker when the job has finished and
  dropped every descriptor and buffer, never when the handler returns. Job slots equal admitted
  requests, so a handler waits only when earlier jobs have outlived their handlers.
- A job carries its kind and validated arguments, a deadline (the handler's remaining processing
  budget), a cancellation flag, a `oneshot` reply channel, and its permit. The queue is a
  `sync_channel(8)`; it can never block the handler because a permit is held before sending.
- Every system call is a step: `openat`, `fstat`, one `read` of at most 65,536 bytes, one
  `readdir` batch of at most 256 entries. Between steps the worker checks the cancellation flag and
  the deadline and stops with `stop_reason: time_budget` or silently when cancelled. Buffers are
  fixed: one 64 KiB read buffer, one result builder bounded by the result cap, two component
  descriptors, one file descriptor.
- Handler protocol: validate arguments; acquire a job slot with a 2 s limit, else tool error `busy`;
  enqueue; await the reply inside the existing `run_with_deadline`. On deadline or client
  cancellation the handler sets the job's cancellation flag, returns `-32603 processing deadline
  exceeded` (deadline) or lets the adapter suppress the output (cancellation), and the job becomes
  orphaned: it stops at its next step boundary, releases its resources and permit, and its reply is
  dropped.
- Accounting published in `Counts` and in diagnostics: `jobs_queued`, `jobs_running`,
  `jobs_orphaned`, `job_slots_available`, `job_descriptors_open`. Bounds: queued plus running at
  most 8; orphaned at most 8; descriptors at most 3 per job plus the root; memory at most 8 times
  (65,536 + 12,288 + 4,096) bytes. Events `job.started`, `job.finished stop_reason=…`,
  `job.orphaned`, `job.busy` carry no paths.

### 4.3 Supervision of stalled steps

A step that never returns (a hung network filesystem, a FIFO masquerading as a file is excluded by
`fstat`, but a stalled volume is not) cannot be cancelled. Each worker owns a transaction marker in
the supervisor's shared state, armed before every step with a 2 s step budget and cleared after it,
exactly like the M0 registry markers. The supervisor, which runs outside the runtime and reads only
atomics, latches `Cause::WorkerStall` (exit 4, `fatal cause=worker_stall`) when a marker exceeds
its budget. An orphaned job therefore ends within one step budget of its cancellation or the
process ends; a handler waiting for a slot waits at most 2 s, which keeps every path inside the 5 s
processing budget.

### 4.4 Shutdown

`close` waits, as in M0, until deliveries are acknowledged, and additionally until
`jobs_queued + jobs_running == 0`, or until a fatal cause, or until the 3 s shutdown budget, where
the supervisor forces exit 1. Exit 0 requires zero requests, zero deliveries, and zero jobs.
Workers are never joined; process exit ends them, and the M0 rule that `main` calls
`std::process::exit` after the serve loop stands, so the runtime's blocking-pool wait cannot occur.

### 4.5 Verification

Fixture tests through the `FileSystem` gate: hold a step past the handler deadline and assert the
`-32603` response, the retired request, `jobs_orphaned = 1`, and the permit still held; submit
eight further slice requests and assert that at most seven run, that the eighth receives `busy` or
waits until the held job is released, and that `job_slots_available` never goes negative; release
the hold and assert the permit returns and descriptors drop to the baseline (lsof count as in M0);
hold a step beyond the step budget and assert exit 4 with `worker_stall` within 2.5 s; EOF with a
held job and assert exit 1 at the shutdown budget; EOF with queued jobs and assert they complete
and the process exits 0. Wire tests confirm a 1 MiB file slice and a 5,000-entry outline complete
within budget without a `worker_stall`.

## 5. Shared policy and frozen contracts (F03)

### 5.1 Shared policy, applied by every tool through one module

- Path arguments: section 3.1 rules; relative only; `""` means the root where a directory is
  expected and is `invalid path` where a file is expected.
- Hidden: a component whose name starts with `.`. Outline and search skip hidden entries unless
  `include_hidden: true`; `file_slice` reads a hidden file when it is not denied (U3 resolved).
- Excluded directories, fixed list: `.git`, `target`, `node_modules`, `.venv`, `__pycache__`.
  Outline lists them with the `excluded` marker and does not descend; search does not descend;
  `file_slice` may read inside them (exclusion governs traversal, denial governs content).
- Denied names, fixed list, matched against the final component: `.env`, `.env.*`, `*.pem`,
  `*.key`, `*.p12`, `*.pfx`, `id_rsa`, `id_rsa.*`, `id_ed25519`, `id_ed25519.*`, `id_ecdsa`,
  `id_ecdsa.*`, `*.kdbx`, `.netrc`, `.npmrc`, `.pypirc`, `credentials`, `credentials.*`,
  `*.keychain`, `*.keychain-db`. Outline lists such entries with the `denied` marker and no size;
  search counts them as skipped; `file_slice` returns `denied path` without opening (U4 resolved
  as this list; D2 asks the reviewer to confirm or amend it).
- Symlinks: never followed anywhere. Outline lists `symlink` without a target; search counts them
  as skipped; `file_slice` on a symlink returns `invalid path`.
- Binary and text: a file is `binary` when its first 8,192 bytes contain a NUL byte; otherwise
  text. Invalid UTF-8 sequences are replaced by U+FFFD and `lossy: true` is set.
- Lines: split on `\n`; a trailing `\r` is removed and `line_endings` reports `lf`, `crlf`,
  `mixed`, or `none`; a final segment without `\n` is a line if non-empty; an empty file has zero
  lines. A line longer than 400 characters is cut at a character boundary and suffixed
  ` [+N chars]`; a search match line is cut at 200 characters the same way.
- Names in text blocks: UTF-8 with lossy replacement; bytes below 0x20 and 0x7F rendered as
  `\xNN`; a backslash rendered `\\`.
- Representation: `content[0]` is one `text` block carrying the compact text form; the structured
  content carries metadata only and never file text, match text, or entry names; the output schema
  describes the metadata object. Codex displays both representations, so duplicating content would
  double the tokens.
- Caps (U5 resolved, both apply): the serialized `result` object (`content`, `structuredContent`,
  `isError`) is at most 12,288 bytes, computed exactly before publication; when the built result
  exceeds it, trailing lines, entries, or matches are removed until it fits and `truncated: true,
  stop_reason: result_cap` is set; the 16,384-byte outbound frame bound remains the hard limit and
  cannot be reached because the envelope adds at most 320 bytes. `max_bytes` on `file_slice`
  bounds the UTF-8 bytes of returned line text after `\r` removal and cutting, excluding prefixes.
- Truncation marker in text: when `truncated` is true, the last text line is `[truncated: <stop_reason>]`.
- Ordering: traversal is depth-first; within a directory entries are sorted by name bytes; outline
  lists entries breadth-first by depth over that traversal; search visits files in traversal order
  and reports matches in line order.

### 5.2 `workspace_outline`

Input schema: object, `additionalProperties: false`; `path` string, max 1,024, default `""`;
`depth` integer 1 to 3, default 2; `max_entries` integer 1 to 200, default 100; `include_hidden`
boolean, default false.

Output schema (structured, all required): `path` string (echo); `entries_listed` integer 0 to 200;
`truncated` boolean; `stop_reason` one of `complete`, `max_entries`, `raw_entry_limit`,
`time_budget`, `result_cap`; `excluded_dirs` integer; `denied_entries` integer; `symlinks`
integer; `hidden_skipped` integer; `other_skipped` integer.

Text grammar: one line per entry, `<kind> <path>[ <bytes>][ excluded|denied]`, where `<kind>` is
`d`, `f`, `l`, or `o` (directory, regular file, symlink, other), `<path>` is root-relative with
`/`, and `<bytes>` appears only for regular files that are not denied. Traversal reads at most
5,000 raw directory entries in total (`raw_entry_limit`) and stops when `max_entries` entries are
listed; the remainder is not counted, so skipped-entry metadata is counters only and its
construction is bounded by the entries actually read. An empty directory yields an empty text block
and `entries_listed: 0`.

### 5.3 `file_slice`

Input schema: `path` string 1 to 1,024, required; `start_line` integer 1 to 10,000,000, default 1;
`max_lines` integer 1 to 400, default 120; `max_bytes` integer 256 to 12,288, default 8,192.

Output schema (structured): required `path`, `start_line`, `returned_lines` (0 to 400), `at_eof`
boolean, `file_bytes` integer, `encoding` (`utf-8` or `binary`), `lossy` boolean,
`line_endings` (`lf`, `crlf`, `mixed`, `none`), `lines_cut` integer, `truncated` boolean,
`stop_reason` one of `complete`, `max_lines`, `max_bytes`, `result_cap`, `time_budget`,
`binary`; optional `end_line` (present when `returned_lines > 0`, equal to
`start_line + returned_lines - 1`) and `total_lines` (present when `file_bytes` ≤ 1,048,576 and
the count completed).

Text grammar: `<line_number><TAB><text>` per line. Binary file: the text block is
`[binary file, <file_bytes> bytes]`, `returned_lines: 0`, `stop_reason: binary`. Start past EOF:
the text block is `[no lines: start_line <n> is past EOF]`, `returned_lines: 0`, `at_eof: true`.
Semantics: lines before `start_line` are skipped by counting while streaming 64 KiB steps; the
first returned line is always returned, cut to `max_bytes` if necessary; a later line that would
exceed `max_bytes` ends the slice with `stop_reason: max_bytes`; `at_eof` is true when the last
returned line is the file's last line; counting for `total_lines` continues to EOF only for files
of at most 1 MiB.

### 5.4 `text_search`

Input schema: `query` string 1 to 128 characters without control characters, required; `path`
string, max 1,024, default `""` (directory or file); `case_sensitive` boolean, default false
(insensitive matching folds ASCII letters only); `max_matches` integer 1 to 100, default 40;
`context_lines` integer 0 to 2, default 0; `include_hidden` boolean, default false.

Output schema (structured, all required): `path`; `query_chars` integer; `matches` integer 0 to
100; `files_scanned`; `files_matched`; `files_skipped` object with integer fields `binary`,
`too_large`, `denied`, `symlink`, `hidden`, `excluded`, `unreadable`, `other`; `bytes_scanned`;
`truncated` boolean; `stop_reason` one of `complete`, `max_matches`, `max_files`, `scan_budget`,
`time_budget`, `result_cap`.

Text grammar: match lines `<path>:<line>:<text>`, context lines `<path>-<line>-<text>`, and `--`
between non-adjacent groups. Bounds: at most 2,000 files (`max_files`), files over 1 MiB skipped as
`too_large`, at most 64 MiB scanned (`scan_budget`), 2 s scanning (`time_budget`), matching per
line after `\r` removal.

### 5.5 Errors

Tool errors (`isError: true`, one fixed message): `missing path`, `invalid path`, `not found`,
`not readable`, `not a file`, `not a directory`, `denied path`, `invalid argument`, `unexpected
argument`, `arguments too large`, `busy`. The M0 argument bound of 4,096 bytes stays. The handler
deadline remains `-32603 processing deadline exceeded`. No message carries content, names, or host
paths.

## 6. Compatibility

Same binary, transport, registry, supervisor, frame and admission bounds, and exit codes, plus exit
4 for `worker_stall`. Without `--root` the wire behaviour is byte-identical to `0dc00896…` except
the version string, verified by the unchanged M0 `tools/list` golden and the full M0 suites; with
`--root`, a second `tools/list` golden lists four tools with `readOnlyHint`, `idempotentHint`,
`openWorldHint: false`, cache hints `ttlMs: 0` for slice and search, `ttlMs: 1000` for outline,
`cacheScope: private`, and `resultType: complete`. Both eras list and call the tools. The probe's
behaviour and bounds are unchanged in both configurations. Through tunnel-client the one-lifecycle
rule and id reuse are unaffected; results stay far below the hosted 10 MiB limit.

## 7. Evidence and correlation for Milestone 1 proofs (F05)

Relay changes (in scope): the known-tool set becomes `connection_probe`, `workspace_outline`,
`file_slice`, `text_search`; `tools/call` request records gain `args_sha256` (16 hex characters of
SHA-256 over the canonical JSON of `arguments`, keys sorted, no whitespace) and `args_bytes`;
response records gain `result_sha256` (16 hex characters over the bytes of `result.content[0].text`),
`result_bytes`, and `is_error`; error responses keep `error_code`. No argument, path, name, or
content is ever recorded. The hook extractor gains `tool_input_sha256` and `tool_response_sha256`
computed the same way from Claude Code's hook payload. The pack extractor records tool inputs and
results as digest plus byte size only.

Correlation per invocation: the harness computes the digests from the client's own record (Claude
Code `tool_use.input` and `tool_result` content; Codex `mcp_tool_call.arguments` and
`result.content[0].text`) and matches each to exactly one relay request and response by tool name,
`args_sha256`, `result_sha256`, and order, including tool errors and protocol errors. The probe
keeps nonce correlation. Canary: the fixture repository contains a directory named
`CANARY_DIR_7f3a` and a file whose content is `CANARY_CONTENT_9c1e`; every retained artifact
(relay records and manifests, hook extracts, tool-events, diagnostics) is scanned for both
sentinels after each proof run, and a relay test feeds synthetic frames containing them and
asserts their absence from records.

## 8. Comparison methodology with attribution controls (F04)

Fixture: this repository at a pinned commit copied to a fresh temporary directory per run (the
canary entries added), no `CLAUDE.md` or `AGENTS.md`, about 10,500 lines across 66 files.

Tasks, fixed wording in a hashed `tasks.json`: T1 describe the top-level layout; T2 state the value
and file of `MAX_OUTBOUND_FRAME_BYTES`; T3 reproduce lines 1 to 60 of `src/main.rs`; T4 list the
test functions in `tests/wire_errors.rs`; T5 list every document mentioning `outbound_too_large`;
T6 reproduce the budgets table from `docs/transport.md`.

Fixed configuration: Claude Code with `--model` pinned to one identifier, `--strict-mcp-config`,
a `--settings` file containing only the evidence hook, `--max-turns 12`, arm-specific
`--allowedTools` and `--disallowedTools`; Codex with `--ignore-user-config --ephemeral`, `-c
model=<id>`, `-c model_reasoning_effort=<fixed>`, read-only sandbox, `approval_policy=never`,
`-C <fixture>`. Client versions and model identifiers are recorded from the clients' own output
(`init.claude_code_version`, `init.model`, `codex --version`, the model field in exec events).

Arms per client: A native tools only, token-saver not configured; B token-saver only, with Claude
Code's `Read`, `Grep`, `Glob`, and `Bash` disallowed, and for Codex an instruction plus an audit
(section below); C both available.

Session isolation: one process per run, fresh fixture copy, fresh relay output directory, fresh
nonce, no resume or session reuse.

Cache state: per cell (client, task, arm) six runs back to back; the first is a warm-up reported
separately and excluded; arm order rotates per task (ABC, BCA, CAB, …) so warm-up and cache effects
are counterbalanced across arms. Cache fields are reported, never folded into a single number.

Usage normalization, without double counting: Claude Code uses `result.usage` only (per-message
usage repeats across content blocks of one message and is never summed): uncached input =
`input_tokens`, cache write = `cache_creation_input_tokens`, cache read =
`cache_read_input_tokens`, output = `output_tokens`. Codex sums `turn.completed.usage` over the
run: total input = `input_tokens`, cached = `cached_input_tokens`, uncached = total minus cached,
output = `output_tokens` (with `reasoning_output_tokens` reported as its component). Raw fields and
these formulas are stored with every run.

Compliance and failures: a run is noncompliant when arm A shows any relay traffic, when arm B on
Claude Code shows any `tool_use` other than the token-saver tools and `ToolSearch`, or when arm B
on Codex shows any `command_execution` or non-MCP tool item; a run is failed on nonzero exit,
timeout at 180 s, `is_error`, max turns, or an incorrect answer. Both are reported per cell with
reasons and excluded from medians; a cell needs five compliant, non-failed runs or is reported as
insufficient after at most ten attempts. No run is "token-saver only" until its audit passes.

Bytes: relay `bytes` are JSON-RPC frames with the envelope and both representations; model-visible
content is the `tool_result` content in Claude Code's stream and the `mcp_tool_call.result`
content in Codex's output; both are reported, and the offline tokenizer estimate (tiktoken, pinned,
`o200k_base`, labelled as an approximation whose correspondence to the current models is
unverified) applies to model-visible content only.

Correctness per task: T1 set overlap of the real top-level names (precision and recall ≥ 0.9); T2
exact value and exact file path; T3 normalized line diff with at least 57 of 60 lines matching in
order; T4 set equality with the parsed test names; T5 set equality with the grep result; T6
row-wise match of budget names and values. A negative fixture, an answer that contains the expected
substrings but is wrong, must fail each checker.

ChatGPT: functional calls of each tool through the tunnel by the operator, payload bytes from the
relay, correctness by the operator's export; token counts Unsupported.

Reporting: medians and ranges per client, task, and arm with compliant-run counts; raw JSON kept
in `docs/evidence/m1/`; nothing aggregated into a single percentage.

## 9. Tests and acceptance criteria

Tests beyond sections 3.4 and 4.5: unit tests for every policy rule and every bound at its limit
and one over; wire tests for both goldens, each tool in both eras, each error, a result at exactly
12,288 bytes, multibyte text and escaping at the cap, long paths, a skipped-only tree, an empty
directory, an empty file, a start past EOF, CRLF and mixed endings, lossy UTF-8, a binary file,
denied and excluded entries, a search that hits each `stop_reason`; fixture canaries for content
and names at every diagnostics level; schema goldens and validation; relay tests for the new
fields, correlation, and sentinels; harness tests for cache accounting, native-tool bypass
detection, the negative correctness fixture, and failed-run reporting.

| ID | Criterion | Evidence and method | Party |
|---|---|---|---|
| M1-01 | Workload confirmed, task set fixed | reviewer decision recorded; hashed task file | reviewer, agent |
| M1-02 | Contracts implemented as frozen in section 5 | schema goldens, wire tests | agent |
| M1-03 | Confinement guarantee demonstrated | section 3.4 deterministic tests, flipper test | agent |
| M1-04 | Blocking work bounded through completion | section 4.5 fixture tests, lsof baseline | agent |
| M1-05 | M0 unchanged without `--root` | M0 suites green, golden unchanged | agent |
| M1-06 | Each tool invoked from Claude Code and Codex and correlated | digest correlation of every call including errors; canaries clean | agent |
| M1-07 | Comparison run with the controls of section 8 | `docs/evidence/m1/` raw runs, compliance audit, medians | agent |
| M1-08 | ChatGPT functional tests and payload bytes | operator run with relay; export | operator |
| M1-09 | Documentation and ADR-0002 | `docs/` | agent |
| M1-10 | Diff, outputs, truthful records, no credentials | commit review | agent, reviewer |

## 10. Dependencies

Runtime: `rustix =1.1.6` (features `std`, `alloc`, `fs`; default features off) and its transitive
`bitflags` and `libc` on macOS; no regex engine; nothing else. Decision D1 may substitute
`cap-std =4.0.3`. Dev: unchanged, plus fixture helpers. Harness: Python 3 standard library and an
optional pinned `tiktoken` in an operator-created virtual environment. Clients as installed on the
run day, versions recorded from their own output. The dependency delta, binary size delta,
`cargo deny` and `cargo audit` results, and notices update are reported with the implementation.

## 11. Risks, assumptions, unresolved requirements

Risks: secrets inside a root reach the model and, through ChatGPT, OpenAI (deny list is a guard,
operator guidance is the control: project roots only, never a home directory, ChatGPT roots only
with content the operator would upload); models over-fetching (caps and arm C); JSON and both
representations inflating results (content appears once, in text); model nondeterminism (five
compliant runs, medians and ranges); Claude Code runtime selection varying per session (both eras
supported and recorded); a stalled filesystem ending the process with exit 4 (documented, the same
posture as M0 stalls); the owned resolver carrying correctness risk (section 3.4 tests; D1).

Assumptions to confirm: the workload; one root per process; ChatGPT file access wanted at all;
Codex's audited instruction accepted as the arm B control.

Unresolved: U1 workload and client priority (recommendation (a) adopted); D1 rustix with the owned
resolver versus cap-std; D2 the exclusion and denial lists; D3 whether `not readable` may be
reported distinctly from `not found` (proposed: yes).

## 12. Exact implementation scope requested for approval

In scope: `src/workspace/` with `confine.rs` (root descriptor, resolver, `FileSystem` trait),
`worker.rs` (pool, slots, steps, accounting), `policy.rs`, `outline.rs`, `slice.rs`, `search.rs`,
and `mod.rs` (tool registration beside the probe); `--root` parsing in `src/main.rs`; worker
markers and `Cause::WorkerStall` in `src/supervisor.rs`; new limits in `src/lib.rs`; diagnostics
events; the tests of sections 3.4, 4.5, and 9; relay and hook-extractor changes of section 7;
`tools/token-compare/` (runner, compliance audit, usage normalizer, checkers, report); documentation
(`docs/tools.md`, `docs/configuration.md`, `docs/transport.md` worker section, `docs/security.md`
T18 to T22 with T22 stalled filesystem, `docs/adr/0002-workspace-root-authority.md`,
`docs/benchmark-methodology.md`, client docs for `--root`); `docs/evidence/m1/` after the runs.
Estimated 2,000 to 2,600 lines of Rust including tests and about 500 lines of Python.

Out of scope: writes, command execution, regex search, symlink following, multiple roots, indexing,
persistent state, ChatGPT token measurement, and any token-saving figure before the comparison has
run.

Decisions requested: U1, D1, D2, D3.
