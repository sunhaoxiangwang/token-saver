# Benchmark methodology

Milestone 0 reports baselines only. Milestone 1 adds a registered comparison (second half of this
document). No token saving is claimed anywhere except as the outcome of that comparison's
pre-registered rule, and no throughput guarantee is made. Numbers are reported separately for the
server alone, the tunnel helper alone, and the complete client path, and only after they were
measured. Results live in `docs/evidence/`.

## Server alone, release binary

| Metric | Method |
|---|---|
| binary size | file size of `target/release/token-saver` after `strip` |
| startup to first response | spawn, send `server/discover`, time to the first stdout line; 20 runs; min, median, max |
| idle RSS and CPU | spawn, idle, sample `ps -o rss=,%cpu=` once per second for 5 s |
| response bytes | frame length of discover, tools/list, and tools/call responses in both eras; `result` object length for a 64-character nonce |
| sequential round trip | 2,000 probe calls over a pipe from a Python driver; p50, p95, p99, max in microseconds; includes the driver's own overhead |
| sustained load | 60 s with 8 requests in flight from a Python driver; RSS and CPU sampled per second; then 10 s drain with RSS sampled |
| handler and delivery time | `response.sent` carries `handler_ms` and `admit_to_delivery_ms` separately |
| SDK bookkeeping | in-process fixture: `num_alive_tasks()` before, during, and after 20 s of load; `cargo test --test fixture_lifecycle -- --ignored bookkeeping --nocapture` |
| EOF to exit | time from closing stdin to process exit |
| pipe saturation | the stall test reports how many bytes the OS pipe absorbed before `EAGAIN` |

The measurement driver is kept outside the repository because it is a session script; its steps are
listed above so they can be reproduced.

## Tunnel helper alone

`ps -o rss=,%cpu=` on `tunnel-client run` while idle polling, with and without the child; `/metrics`
snapshot. Not executed in Milestone 0: requires the operator's tunnel and key.

## Complete client paths

| Path | Metric | Source |
|---|---|---|
| Claude Code | tool call duration as the client saw it | PostToolUse hook `duration_ms` |
| Claude Code | server-side request in to response out | relay record `elapsed_ms` difference |
| Codex | server-side request in to response out | relay record `elapsed_ms` difference |
| ChatGPT | server-side hop | relay records; end-to-end only by stopwatch, labelled approximate |

## Interpreting threshold misses

A task-count or RSS threshold miss is reported with its measurements first. Attribution to the SDK
requires isolating the cause from application retention, allocator behaviour, and instrumentation,
and repeating the load and drain cycles. The alive-task counter counts tokio tasks on the runtime
only; threads, kernel buffers, and allocator caches are outside its coverage.

## Milestone 1 comparison (`tools/token-compare/`)

The design is TS-M1-PLAN-001 revision 4, section 8; the harness is `tools/token-compare/` and its
tests `tools/token-compare/tests/`.

| Element | Method |
|---|---|
| Fixture | this repository at a pinned commit, copied fresh per attempt, with the canary entries `CANARY_DIR_7f3a/` and `canary.txt` (`CANARY_CONTENT_9c1e`) added; no `CLAUDE.md` or `AGENTS.md` |
| Tasks | T1 to T6 with fixed wording in `tasks.json`, whose SHA-256 is in every record |
| Clients | Claude Code (`claude -p --model <pinned> --strict-mcp-config --mcp-config --settings --max-turns 12 --output-format stream-json`), Codex (`codex exec --ignore-user-config --ephemeral --json -c model=<pinned> -c model_reasoning_effort=<pinned> -c sandbox_mode="read-only" -c approval_policy="never"`); versions read back from client output |
| Arms | A native tools only (no MCP server configured); B token-saver only (Claude Code: `Read`, `Grep`, `Glob`, `Bash`, `LS`, editing and web tools disallowed; both clients: an instruction sentence plus an audit); C both |
| Attempts | exactly seven per cell, attempt 0 a warm-up reported separately; no retries, no top-up; arm order rotates per task (ABC, BCA, CAB) |
| Isolation | one process per attempt, fresh fixture, fresh relay directory, fresh nonce, no session reuse |
| Usage | Claude Code `result.usage` only: `input_tokens` (uncached), `cache_creation_input_tokens`, `cache_read_input_tokens`, `output_tokens`; Codex sum of `turn.completed.usage`: `input_tokens` (includes cached), `cached_input_tokens`, `cache_write_input_tokens`, `output_tokens` (includes reasoning). `total_tokens` is the complete unweighted sum of every category once; every category is shown beside it; no cost is computed |
| Compliance | A: no relay traffic and no MCP call; B: Claude Code only `ToolSearch` and `mcp__token-saver__*` tool uses, Codex no `command_execution` or non-MCP item and only `mcp_tool_call` items with server `token-saver` and one of the three tools; C unconstrained |
| Attribution | requires complete correlation: every client call matched by `(tool, args_sha256)` in order to a relay request whose response digest agrees (or an error on both sides); otherwise `unverified` |
| Correctness | T1 precision and recall at least 0.9 over top-level names; T2 exact value and path; T3 exact normalized reproduction of the 60 lines, else `partial` with the matching-line count; T4 and T5 set equality; T6 exact row match; `no_answer` when nothing came back |
| Reporting | per cell: completed, compliant, attributed, correct, partial, usage unavailable, out of six; failure-inclusive sums and medians per category and `total_tokens`; `total_tokens` per correct answer; success-conditioned medians labelled with their count; resolution (`unresolved` unless measured attempts 1 to 6 are present exactly once and every one has trusted usage; a failed attempt reporting zero tokens is treated as usage unavailable) |
| Claim rule | a saving is stated only when both cells are resolved, every arm B attempt is compliant with verified attribution and made at least one token-saver call, arm B's correct count is at least arm A's, arm B's `total_tokens` per correct answer is lower, and the success-conditioned medians agree; the statement is "fewer total tokens per correct answer" with every category's delta beside it; otherwise "no saving established" or "unresolved" |
| Bytes | `client_recorded_result_bytes` (Claude Code `tool_result` content, Codex `mcp_tool_call.result` text), `relay_result_text_bytes`, `relay_response_frame_bytes`; none is called model-visible. Codex formats and truncates tool results before the model sees them (`core/src/tools/context.rs` at `rust-v0.167.0`) |
| Retention | raw streams hashed and deleted after extraction; attempt records keep digests, sizes, usage, verdicts; every retained file scanned for the canaries, the fixture path, the home directory, and e-mail addresses; relay manifest argv paths replaced by placeholders |
| ChatGPT | functional calls and relay payload bytes only, by the operator; token counts Unsupported |
