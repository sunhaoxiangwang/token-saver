# TS-M1-PLAN-001, Milestone 1 plan, revision 1 (plan only)

Submitted 2026-11-06 against the committed Milestone 0 tree (`main` at `c11ed69`, release binary
`0dc00896…`). Nothing in this document is implemented. Token savings are not claimed; the plan
defines how they will be measured.

## 0. Inputs, the open requirement, and the working assumption

Verified facts:

- No Milestone 1 specification exists in the repository, in the conversation history, or in the
  M0 assignments. The M0 assignment excluded filesystem tools, command execution, indexing,
  databases, model API integration, and UI for the proof only; it did not define what follows.
- The committed server exposes one tool and makes no token-saving claim. Its transport, registry,
  supervisor, bounds, dual-era handling, and evidence tooling are approved and tested (87 tests).
- Both local clients report token usage per run in machine-readable form: Claude Code
  `stream-json` carries `usage` on every assistant message and on the final `result`
  (`input_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`, `output_tokens`);
  Codex `exec --json` carries `turn.completed.usage` (`input_tokens`, `cached_input_tokens`,
  `output_tokens`). In the recorded single-probe sessions the fixed context dominated: Claude Code
  run 2 read 62,354 cached tokens and 66 uncached input tokens for a 382-byte tool result; Codex
  used 29,868 input tokens, 27,008 of them cached, for a 358-byte result. Any saving therefore has
  to be measured as a difference between arms on identical tasks, with cache fields kept separate,
  not as a share of total context.
- ChatGPT Web exposes no token counts to the user. Only payload bytes can be measured there.
- The process-listing remark in the assignment is an example of a missing capability, not a
  feature request, and is not proposed here.

Open requirement (one question, asked with this submission): which workload should Milestone 1
reduce tokens for, and in which client does the spend that matters occur? The candidates are
(a) inspecting code and documents inside a project directory from Claude Code and Codex,
(b) digesting developer-command output such as test and build logs, (c) local machine facts for
ChatGPT Web, or something else.

Working assumption: (a). It is the workload all three clients perform, it is the only one whose
baseline the clients' own tools already produce in measurable form, and it keeps the server
read-only with no execution authority. Every section below is written for (a); a different answer
changes sections 2 to 7 and will be resubmitted as revision 2.

## 1. Intended workflow and measurable benefit

Workflow: an assistant answering a question about a project ("where is X defined", "show me the
budget table", "what is in this directory") fetches a directory outline, a bounded slice of a
file, or search hits, and answers from them. Today Claude Code and Codex do this through their
native tools or a shell (`ls -R`, `cat`, `grep -rn`), whose output enters the context unbounded
except by the client's own truncation. ChatGPT Web cannot do it at all without the operator
pasting content.

Benefit, as a measurable quantity: for a fixed task set on a fixed fixture repository, the
client-reported uncached input tokens and output tokens per task, the bytes of tool results that
enter the context, the number of tool calls, and task correctness, compared between a baseline arm
(native tools only) and a token-saver arm (token-saver tools only). The claim form after
measurement is "task T on client C used N fewer uncached input tokens (median of five runs), with
correctness unchanged", never a percentage without its task and client.

## 2. Proposed scope and alternatives

Proposed: a Workspace Reading Pack of three read-only tools over one operator-declared root, plus a
reproducible comparison harness. No writes, no execution, no network, no persistent state, no
indexing.

| Alternative | Benefit | Cost | Decision |
|---|---|---|---|
| (a) Workspace Reading Pack | bounded, compact file and directory views for all three clients; baseline measurable with native tools | file-read authority under a root; secret exposure risk inside the root | proposed |
| (b) Command-output digests (allowlisted commands, e.g. test runners) | largest raw volume to compress for Claude Code and Codex | execution authority, output parsing per tool, allowlist governance | deferred; revisit after (a) is measured |
| (c) Host facts (process list, disk, network) | fills a ChatGPT capability gap | little to compress; host identifiers leave the machine; explicitly not approved | not proposed |
| (d) Notes or context store | avoids re-sending content across turns | persistent state, a design the M0 architecture excludes; savings depend on usage habits | not proposed |
| (e) No new tools; measure clients' native output only | zero authority change | no token-saving functionality | not proposed |

## 3. Tools and contracts

All three tools are advertised only when the server is started with `--root <abs-dir>`. Without
it, `tools/list` is identical to Milestone 0. Paths in arguments are relative to the root, UTF-8,
at most 1,024 bytes, with no absolute form, no `..` component, and no NUL. Every tool returns
structured content plus one text block, with `resultType: complete`; serialized results are capped
at 12,288 bytes so that every frame stays under the 16,384-byte outbound bound with headroom. The
probe keeps its 1,024-byte result bound.

### 3.1 `workspace_outline`

Input: `path` (string, default `""` meaning the root), `depth` (integer 1 to 3, default 2),
`max_entries` (integer 1 to 200, default 100), `include_hidden` (boolean, default false).

Output: `root_relative_path`, `entries` as an array of `{path, kind, bytes}` where `kind` is
`file`, `dir`, `symlink`, or `other` and `bytes` is present for files; `total_entries_seen`,
`truncated` (boolean), `omitted_dirs` (names skipped by the fixed exclusion list: `.git`,
`target`, `node_modules`, `.venv`, `__pycache__`). Symlinks are reported, never followed. The text
block is one line per entry: `kind path bytes`.

### 3.2 `file_slice`

Input: `path` (string, required), `start_line` (integer ≥ 1, default 1), `max_lines` (integer 1 to
400, default 120), `max_bytes` (integer 256 to 12,288, default 8,192).

Output: `path`, `start_line`, `end_line`, `total_lines` (present when the whole file was counted
within a 1 MiB read bound, else absent), `truncated`, `encoding` (`utf-8` or `binary`), and the
text block `N<TAB>line` for each returned line, lines over 400 characters cut with a marker. A
file detected as binary (NUL in the first 8 KiB or invalid UTF-8 beyond lossy repair) returns
`encoding: binary` and no content. Files matching the fixed secret deny list (`.env`, `.env.*`,
`*.pem`, `*.key`, `*.p12`, `*.pfx`, `id_rsa*`, `id_ed25519*`, `*.kdbx`, `.netrc`, `.npmrc`,
`.pypirc`, `credentials*`, `*.keychain*`) return a tool error `denied path` without opening the
file.

### 3.3 `text_search`

Input: `query` (string, 1 to 128 characters, literal, no regex), `path` (string, default `""`),
`case_sensitive` (boolean, default false), `max_matches` (integer 1 to 100, default 40),
`context_lines` (integer 0 to 2, default 0).

Output: `matches` as `{path, line, text}` with `text` cut at 200 characters, `files_scanned`,
`files_skipped` with reasons (`binary`, `too_large`, `denied`, `excluded`), `truncated`, and
`stop_reason` (`complete`, `max_matches`, `time_budget`, `scan_budget`). The text block is
`path:line: text` per match.

### 3.4 Errors

Argument failures are tool errors with fixed messages (`missing path`, `invalid path`, `path
outside root`, `not found`, `denied path`, `not a file`, `not a directory`, `invalid argument`,
`arguments too large`), following the M0 error contract. No message includes file content or
absolute host paths.

## 4. Authority and resource bounds

Authority comes only from the launch command: `token-saver serve --root /abs/dir`, written by the
operator into the client configuration, exactly as the M0 binary path is. No environment variable,
configuration file, or client-provided `roots` is consulted; client identity is not authorization.
One root per process in this milestone. Resolution: lexical rejection of absolute paths and `..`,
then `canonicalize` of root and target, then a prefix check of the canonical target against the
canonical root, then opening the file and checking by `metadata` that it is a regular file or
directory. Residual TOCTOU on a symlink swapped between check and open is documented; closing it
needs `O_NOFOLLOW`, which requires the `libc` crate at runtime (decision requested in section 10).

Fixed bounds, enforced per call and documented in `docs/configuration.md`: path argument ≤ 1,024
bytes; outline ≤ 200 entries, depth ≤ 3, directory read stops at 5,000 raw entries; slice ≤ 400
lines, ≤ 12,288 bytes, line counting reads at most 1 MiB; search ≤ 100 matches, ≤ 2,000 files,
≤ 1 MiB per file, ≤ 64 MiB total scanned, ≤ 2 s scanning time inside the existing 5 s processing
budget; serialized result ≤ 12,288 bytes with truncation flags, never a fatal. File work runs on
the Tokio blocking pool with at most eight concurrent operations, which the existing admission
permits already guarantee; all reads are bounded so a blocked thread cannot outlive the deadline by
more than one bounded read. Memory per call stays under 2 MiB (1 MiB read buffer plus result).
Diagnostics gain allowlisted events (`tool.denied reason=…`, `tool.truncated reason=…`) that never
carry paths or content; canary tests extend to file content and names.

Threat-model additions for `docs/security.md`: T18 secret exposure inside the root (deny list,
operator guidance to declare project roots and never a home directory; ChatGPT roots must contain
nothing the operator would not upload to OpenAI); T19 path escape (lexical and canonical checks,
symlink policy, tests); T20 resource exhaustion through many large files (scan budgets); T21 result
flooding of the model context (hard result caps and truncation flags).

## 5. Compatibility with the existing server and clients

Same binary, same transport adapter, registry, supervisor, bounds on frames and admissions, and
exit codes; the three tools are added beside the probe behind `--root`. Both eras list and call
them; `tools/list` grows from one to four entries and stays under the outbound bound. Annotations:
`readOnlyHint`, `idempotentHint`, `openWorldHint: false` on all three. Cache hints: `ttlMs: 0`
for slice and search, `ttlMs: 1000` for outline, `cacheScope: private`. Through tunnel-client the
one-lifecycle-at-a-time behaviour is unchanged. Codex's 60 s tool timeout and the clients' result
handling are unaffected because results are far below their limits. M0 wire suites must pass
unchanged with and without `--root`; without `--root` the wire behaviour is byte-identical to the
committed binary except for the version string.

## 6. Tests and acceptance criteria

Tests (all temporary fixtures, created and removed per test):

- unit: path resolution (absolute, `..`, NUL, long path, symlink inside and outside root,
  canonical prefix on a root that is itself a symlink), binary detection, line cutting,
  deny-list matching, every bound at its limit and one over, result-size capping.
- wire: `tools/list` without `--root` equals the M0 golden; with `--root` the new golden; each tool
  called in both eras; every error message; a result at exactly the 12,288-byte cap; a search
  that hits the time budget on a generated tree; concurrency of eight slices under load.
- fixture: no file content or path in diagnostics at every level (canary extension); supervisor
  budgets unaffected by a blocking read of a 1 MiB file.
- schema: golden `tools/list` for both configurations; JSON Schema validation of inputs and
  outputs as in M0.
- harness: the comparison scripts' manifest, hashing, and parsing tested against recorded runs.

Acceptance criteria (evidence, method, responsible party):

| ID | Criterion | Evidence and method | Party |
|---|---|---|---|
| M1-01 | Workload confirmed and task set fixed | reviewer answer recorded in this plan's revision 2; task set file hashed | reviewer, agent |
| M1-02 | Tool contracts implemented as specified | schema goldens, wire tests | agent |
| M1-03 | Authority limited to the declared root | escape tests, symlink tests, deny-list tests, security doc | agent |
| M1-04 | Every bound enforced, no fatal from tool results | bound tests, stall and load fixtures | agent |
| M1-05 | M0 behaviour unchanged without `--root` | M0 suites green, golden unchanged | agent |
| M1-06 | Automated proofs from Claude Code and Codex calling each tool | relay records, client tool events, hook extract, nonce correlation as in M0 | agent |
| M1-07 | Comparison run per section 7 with raw per-run data | `docs/evidence/m1/` manifests and usage tables | agent |
| M1-08 | ChatGPT path | payload-bytes measurement only, when the tunnel proof exists | operator |
| M1-09 | Documentation and ADR-0002 (authority model) | `docs/` | agent |
| M1-10 | Diff, outputs, truthful records, no credentials | commit review | agent, reviewer |

## 7. Reproducible comparison methodology

Fixture: this repository at a pinned commit, copied to a temporary directory (so the fixture is
public, stable, and large enough: about 10,500 lines across 66 files).

Task set (fixed wording, stored as `tools/token-compare/tasks.json` and hashed): T1 describe the
top-level layout; T2 state the value of `MAX_OUTBOUND_FRAME_BYTES` and its file; T3 reproduce lines
1 to 60 of `src/main.rs`; T4 list the test function names in `tests/wire_errors.rs`; T5 list every
document mentioning `outbound_too_large`; T6 reproduce the budgets table from `docs/transport.md`.
Each task has a deterministic correctness check (exact strings that must appear).

Arms per client: A baseline, native tools only, token-saver not configured; B token-saver tools
only, native file and shell tools disallowed where the client supports it (Claude Code
`--disallowedTools` for `Read`, `Grep`, `Glob`, `Bash`; Codex read-only sandbox with an
instruction to use the server, recorded as a weaker control); C both available, to observe which
the model chooses.

Measures per run: client-reported usage fields as emitted (uncached input, cache creation, cache
read, output), number of tool calls and which tools, bytes of tool results (from the relay for
token-saver, from the client's `tool_result` blocks for native tools), wall time, correctness
pass or fail. Five runs per task, arm, and client; medians and ranges reported; raw JSON kept.
Model identifiers, client versions, server hash, fixture hash, and prompt text are recorded in a
manifest. An offline tokenizer estimate (tiktoken, pinned version, `o200k_base`) of payload bytes
is reported as an approximation only, labelled with the fact that its correspondence to the
current models is unverified; client-reported counts are the primary measure. ChatGPT: bytes only.

Reporting rule: numbers appear only after the runs exist, as medians with ranges per task and
client, never aggregated into one percentage.

## 8. Dependencies

Server: no new runtime crate for the proposed tools (standard library file APIs); `libc` becomes a
runtime dependency only if `O_NOFOLLOW` is approved (it is already a dev-dependency, MIT or
Apache-2.0, and already audited). No regex engine; search is literal. Comparison harness:
Python 3 standard library plus optional `tiktoken` in an operator-created virtual environment,
version pinned when installed; its license (MIT) added to the notices if it is vendored, which is
not planned. Client versions as installed on the day of the runs, recorded from the clients' own
output (Claude Code `init` line, `codex --version`).

## 9. Risks, assumptions, unresolved requirements

Risks: a root that contains secrets exposes them to the model and, through ChatGPT, to OpenAI (deny
list is a guard, not a guarantee; operator guidance is the control); models may fetch more than
they need (hard caps bound the damage; arm C measures the behaviour); results in JSON carry
overhead that could erase savings for small files (the text block is the compact form; the
comparison will show it); nondeterministic model behaviour (five runs, medians and ranges);
Claude Code's runtime selection varies between sessions (both eras are supported and recorded);
tokenizer estimates may mislead (secondary, labelled).

Assumptions to confirm: the workload (section 0); that a single root per process is acceptable;
that ChatGPT file access is wanted at all given data transit; that Codex's lack of a tool
disallow switch is an acceptable weaker control for arm B.

Unresolved requirements: U1 workload and client; U2 `O_NOFOLLOW` via `libc` or documented
residual risk; U3 whether hidden files are readable by `file_slice` when not listed by default
(proposed: yes, subject to the deny list); U4 the exact fixed exclusion and deny lists; U5 whether
a 12,288-byte result cap or the full 16,384-byte frame bound should govern.

## 10. Exact implementation scope requested for approval

In scope, pending the workload answer: `src/workspace.rs` (path authority, the three tools,
bounds), `--root` parsing in `src/main.rs`, tool registration beside the probe, new limits in
`src/lib.rs`, allowlisted diagnostics events, unit, wire, fixture, and schema tests listed in
section 6, `tools/token-compare/` harness (Python, dev-only) with its tasks file and manifest
format, documentation (`docs/tools.md`, `docs/configuration.md`, `docs/security.md` T18 to T21,
`docs/adr/0002-workspace-root-authority.md`, `docs/benchmark-methodology.md` comparison section,
client docs for `--root` configuration), and `docs/evidence/m1/` after the runs. Estimated size:
about 1,200 to 1,600 lines of Rust including tests, about 300 lines of Python.

Out of scope: writes, command execution, regex search, multiple roots, indexing, persistent
state, ChatGPT token measurement, and any token-saving figure before the comparison has run.

Decisions requested: the workload (U1); `libc` for `O_NOFOLLOW` (U2); the result cap (U5).
