# TS-M1-PLAN-001, Milestone 1 plan, revision 3 (plan only)

Submitted 2026-11-06 against `main` at `013734d` (release binary `0dc00896…`). Nothing in this
document is implemented. Token savings are not claimed; the plan defines how they are measured.

Changes from revision 2, per TS-M1-REVIEW-002. The review's positions on U1, D1, D2, and D3 are
recorded (section 0). F01: every object is classified before it is opened, every open is
non-blocking, a job marker bounds the lifetime of orphaned work, and every supervision test states
its timeline and the cause expected to win (3.1, 4.3, 4.5). F02: the subtree claim is withdrawn and
replaced by an explicit retained-directory authority contract whose move-out behaviour is documented
and tested (3.2, 3.3, 3.5). F03: partial lines, result capping, metadata recomputation, and traversal
storage are specified (4.2, 5.6). F04: the comparison runs a fixed number of attempts per cell and
reports failure-inclusive denominators, exact reproduction, and byte metrics named by their
observable source (section 8). F05: temporary raw traces are separated from retained artifacts, with
canonicalization vectors and correlation rules for errors and repeated calls (section 7).

## 0. Inputs, recorded decisions, and the open requirement

Verified facts are unchanged: no Milestone 1 specification exists; the M0 server exposes one tool
and claims no savings; Claude Code and Codex report usage per run in machine-readable form, and in
the recorded single-probe sessions the fixed context dominated (Claude Code run 2: 62,354 cache-read
tokens and 66 uncached input tokens for a 382-byte result; Codex: 29,868 input tokens, 27,008
cached, for a 358-byte result); ChatGPT exposes no counts, used the modern era through the tunnel,
and reused request ids across requests (`docs/evidence/m0/chatgpt.relay.records.jsonl`).

Decisions recorded from TS-M1-REVIEW-002:

| Item | Review position | Effect in this revision |
|---|---|---|
| U1 | continue planning around workload (a) with separate Claude Code and Codex results; the user's spending priority remains unconfirmed | sections 1 to 9 stand; a different answer from the user becomes revision 4 |
| D1 | rustix acceptable in principle; cap-std not required | `rustix =1.1.6` proposed (3.4); cap-std stays a documented alternative |
| D2 | the exclusion and denial lists are acceptable as documented guards, without a guarantee that they remove all secrets | lists frozen in 5.1 and documented as guards; operator guidance in section 11 is the control |
| D3 | reporting `not readable` separately from `not found` is acceptable within the declared authority | kept in 5.5 |

Reviewer reproductions, repeated here on macOS 25.6.0 on 2026-11-06 with the flags this plan
proposes: a FIFO opened `O_RDONLY` blocked past 3 s; opened `O_RDONLY|O_NONBLOCK` it returned at
once and `fstat` reported a FIFO; opened with `O_DIRECTORY` it returned `ENOTDIR` at once; a Unix
socket open returned `EOPNOTSUPP`; `fstatat(..., AT_SYMLINK_NOFOLLOW)` classified the FIFO without
opening it. A directory opened through the root descriptor, renamed outside the root, then given a
new file, served that file through the retained descriptor, while a fresh `openat` of the same name
from the root returned `ENOENT` and the moved directory's `..` no longer matched the root's
identity. These observations shape 3.1 and 3.2.

## 1. Workflow and measurable benefit

An assistant answering a question about a project fetches a directory outline, a bounded slice of a
file, or literal search hits, and answers from them. Today Claude Code and Codex do this through
native tools or a shell whose output enters the context bounded only by the client's truncation;
ChatGPT Web cannot do it without pasted content. The benefit is measured per client, task, and arm
as client-reported usage, tool-result bytes at named observation points, tool-call counts, and
correctness (section 8). The claim form after measurement is "task T on client C: arm B answered
correctly in k of 6 attempts against arm A's m of 6; failure-inclusive median uncached input N
versus M tokens; expenditure per correct answer P versus Q tokens", never a bare percentage.

## 2. Scope and alternatives

Proposed: a Workspace Reading Pack of three read-only tools over one operator-declared root, an
owned worker pool, descriptor-relative resolution with an explicit authority contract, relay and
harness extensions for correlation, and a comparison harness. No writes, execution, network,
persistent state, indexing, or symlink following.

| Alternative | Benefit | Cost | Decision |
|---|---|---|---|
| (a) Workspace Reading Pack | bounded, compact file and directory views for all clients; measurable baseline | file-read authority under a root; secret exposure inside the root; one new dependency | proposed, recommended by review |
| (b) Command-output digests | largest raw volume to compress | execution authority, per-tool parsing, allowlist governance | deferred until (a) is measured |
| (c) Host facts | fills a ChatGPT capability gap | little to compress; host identifiers leave the machine; not approved | not proposed |
| (d) Notes or context store | avoids re-sending content | persistent state, excluded by the M0 architecture | not proposed |
| (e) No new tools | no authority change | no token-saving functionality | not proposed |

## 3. File authority and special files (F02, F01)

### 3.1 Mechanism

Root descriptor. At startup, before serving, `main` requires `--root <absolute path>`, opens it
once with `rustix::fs::open(path, RDONLY | DIRECTORY | CLOEXEC | NOFOLLOW | NONBLOCK | NOCTTY)`,
confirms a directory with `fstat`, and hands the `OwnedFd` to the worker pool, which holds it for
the process lifetime. This is the only use of ambient path authority. The textual root never
appears in results or diagnostics.

Argument validation (lexical, in the handler, no I/O). A path argument is UTF-8, 1 to 1,024 bytes,
contains no NUL, does not start with `/`, splits on `/` into 1 to 64 components, each 1 to 255
bytes and neither `.` nor `..`. The empty string denotes the root where a directory is expected.
Anything else is `invalid path` before any descriptor is touched.

Classification before any open. The resolver calls `statat(current, name, AtFlags::SYMLINK_NOFOLLOW)`
on every component and on every directory entry whose `DirEntry::file_type()` is `Unknown` before
it opens anything. `stat` never blocks on a FIFO. Symlink: `invalid path`, never opened. Regular
file where a directory is required: `not a directory`. Directory where a file is required:
`not a file`. FIFO, socket, character device, block device, or unknown: `other`, never opened
(`not a file` for `file_slice`; counted and listed as `o` by the traversal tools). `ENOENT`:
`not found`; `EACCES`: `not readable` (D3); `ENOTDIR` on an ancestor: `not a directory`.

Non-blocking opens. Every open is `openat(current, name, RDONLY | CLOEXEC | NOFOLLOW | NONBLOCK |
NOCTTY)`, plus `DIRECTORY` for ancestors and directory targets. `NONBLOCK` makes a FIFO open return
immediately instead of waiting for a writer (reproduced, section 0); a socket open fails
(`EOPNOTSUPP` on macOS, `ENXIO` on Linux); `NOCTTY` prevents acquiring a controlling terminal if a
terminal device were ever opened. The previous component's descriptor is closed once the next is
open.

Post-open check, authoritative. `fstat` on the new descriptor; `FileType::from_raw_mode(st_mode)`
must be `RegularFile` or `Directory` as the tool requires, otherwise the descriptor is closed unread
and the result is `not a file` or `not a directory`. For a regular file the worker then clears
`NONBLOCK` with `fcntl_setfl(fd, fcntl_getfl(fd)? - NONBLOCK)`, so reads proceed under the
specified blocking semantics of regular files; a stalled volume is then the step-budget case of
4.3. The pre-open classification avoids opening objects already visible as special files; the
post-open check catches a replacement between the two calls; `NONBLOCK` makes that window harmless
for FIFOs, which any user can create; device nodes cannot be created without privilege.

Listing. `rustix::fs::Dir::read_from(&dir_fd)` is used for directory entries. In rustix 1.1.6's
libc backend it opens an independent descriptor (`fcntl_getfl`, then `openat(fd, ".", flags |
CLOEXEC)`, then `fdopendir`), so an open iterator owns one additional descriptor and one libc `DIR`
buffer. The worker reads a directory's entries completely, up to the per-directory limit of 5.6,
into memory, then drops the iterator before it touches any child. Children are opened by the same
`statat` then `openat` rule.

Holding windows per tool, which section 3.2 depends on:

- `file_slice`: directory descriptors live only during the walk; once the file is open every
  directory descriptor is dropped and all reads use the file descriptor alone.
- `workspace_outline`: each directory visited is re-resolved from the root by its path (at most
  three components), listed completely, sized by `statat` per regular-file entry, and dropped
  before the next directory is visited.
- `text_search`: each directory visited is re-resolved from the root by its path (at most 64
  components), listed completely, then its regular-file entries are opened one at a time with the
  directory descriptor held; it is dropped before the next directory is visited.

### 3.2 Authority contract (replaces the revision 2 guarantee)

The revision 2 statement that no open can be directed "outside the subtree of the directory the
root descriptor refers to" is withdrawn; the reviewer's move-out sequence disproves it, and this
plan reproduces it. The mechanism provides retained-directory authority, stated as follows.

Definitions. R is the directory object opened at startup. A job holds a directory object when it
holds a descriptor to it. A job obtains a directory object only by a single-component, no-follow,
descriptor-relative open of an entry of a directory object it already holds, starting from R.

Contract. (1) A job opens an object only if, at the instant of the open, it is a direct child of a
directory object the job holds; symlink entries are never opened; entries that are neither regular
files nor directories are never opened. (2) Descriptors refer to objects, not pathnames: a held
directory object stays held after it is renamed, including a rename out of R's current subtree, and
R stays held for the process lifetime wherever it is renamed. (3) A job holds non-root directory
objects only within the windows of 3.1 and never beyond the job's lifetime, which is bounded by
PROCESSING + JOB_GRACE = 7.5 s after admission (4.3), or the process exits with code 4. (4) Every
resolution starts from R and sees the tree as it is: a directory moved out of R is `not found` to
the next job and to the next directory visit of a traversal.

Unreachable by construction: any object named through `..`, through an absolute path, or through a
symlink; any object that was never, at the instant of some open in a job's chain, a direct child of
a directory object that job held.

Move-out behaviour, documented. If a directory object held by a running job is renamed out of R's
subtree while held, that job continues to read the directory's current contents, including entries
created or moved into it after the rename, until it drops the descriptor: for `file_slice` only
until the file is open; for `workspace_outline` and `text_search` until that directory's visit
ends. New jobs cannot reach it. The actor who can perform this rename has write permission on the
root directory; the same actor can move or link the same content into the root directly, which is
the move-in residual below. Move-out therefore adds no content that the move-in residual does not
already expose, but it changes where the content appears to other observers, and it is documented
and tested (C8) for that reason.

Residuals, documented, not escapes: content moved or hard-linked into R or into a held directory
becomes readable; a mount point inside R is inside; a renamed R is followed by its descriptor; a
deleted R resolves children to `not found`.

### 3.3 Stronger containment considered

- Identity walk: after a resolution, open `..` repeatedly from the deepest held directory, compare
  `(st_dev, st_ino)` with the parent chain and finally with R. Reproduced here: after the move-out,
  `..` of the moved directory no longer matched R, so the walk detects a completed move-out at the
  time of the check. It does not detect a move that happens after the check, costs up to 64 extra
  opens and `fstat`s per resolution and per directory visit, and changes the window rather than the
  contract. Not proposed; offered as optional hardening D4, which would add one step kind and
  reuse the tests of 3.5.
- Linux `openat2` with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` (kernel 5.6 and later) detects
  concurrent renames during one lookup and fails with `EAGAIN`; it does not cover retained
  descriptors and is unavailable on macOS, where M1 is verified. Out of scope.
- `cap-std =4.0.3`: D1, not required; its `Dir` follows symlinks that stay inside the sandbox,
  which this plan does not want.

### 3.4 Dependency and `forbid(unsafe_code)` implications

The standard library exposes no `openat` or `fstatat`, so descriptor-relative resolution needs a
system-call crate. Proposed: `rustix = "=1.1.6"`, `default-features = false`, features `std`,
`alloc`, `fs` (repository bytecodealliance/rustix; license `Apache-2.0 WITH LLVM-exception OR
Apache-2.0 OR MIT`, accepted by `deny.toml` through the `Apache-2.0` and `MIT` alternatives). Items
used, all confirmed in the 1.1.6 documentation: `open`, `openat`, `statat`, `fstat`, `fcntl_getfl`,
`fcntl_setfl`, `Dir::read_from`, `DirEntry::{file_name, file_type}`, `OFlags::{RDONLY, DIRECTORY,
CLOEXEC, NOFOLLOW, NONBLOCK, NOCTTY}`, `AtFlags::SYMLINK_NOFOLLOW`, `FileType::from_raw_mode` with
variants `RegularFile`, `Directory`, `Symlink`, `Fifo`, `Socket`, `CharacterDevice`,
`BlockDevice`, `Unknown`, and `Stat::st_mode`. All are safe functions over `OwnedFd` and `AsFd`;
rustix contains `unsafe` FFI internally, and the token-saver crate keeps `#![forbid(unsafe_code)]`
unchanged. Transitive additions on macOS are `bitflags` and `libc` (already a dev-dependency), on
Linux `linux-raw-sys`; the exact set, the binary size delta, `cargo deny`, `cargo audit`, and the
third-party notices are reported with the implementation. The resolver is about 200 lines owned by
this project and tested per 3.5.

### 3.5 Verification

The worker's system calls sit behind a `FileSystem` trait: the rustix implementation in production,
and a fixture wrapper in `tests/` that can hold inside any call (the step marker stays armed) or at
the checkpoint after any call (markers clear), the same pattern as the M0 `GateService`. Each test
asserts that no outside content is returned except where the contract says it is, that the expected
error or content appears, and that `job_descriptors_open` returns to zero.

- C1: pre-existing symlinked ancestor, symlinked final component, symlinked root argument: `invalid path`.
- C2: hold after opening `a`, replace `a/b` with a symlink to an outside directory, resume: `invalid path`.
- C3: hold after opening `a/b`, replace `a` with a symlink to an outside directory, resume: the job
  reads the original `a/b/file` through the held descriptor (contract clause 2).
- C4: hold after the directory open, swap the final file for a symlink, resume: `invalid path`.
- C5: rename the root after startup and create a new directory with a sentinel file at the old
  path: tools keep reading the original tree; the sentinel is unreachable.
- C6: lexical rejections: `..`, `.`, empty component, leading `/`, NUL, 65 components, a 256-byte
  component, a 1,025-byte path.
- C7: a flipper thread alternating a directory between real and symlink-to-outside for two seconds
  under continuous reads: zero outside content; every result is original content or `invalid path`.
- C8, move-out and create (new): `file_slice a/b/file`; hold after opening `a`; rename `root/a` to
  `outside/a`; create `outside/a/b/file` containing sentinel S1; resume. Expected under the
  contract: the job returns S1, `job_descriptors_open` returns to zero at job end; a second
  `file_slice a/b/file` returns `not found`; `workspace_outline ""` no longer lists `a`. The same
  interleaving during a `text_search` directory visit: files created in the moved directory after
  the move are scanned by that visit only, and a second search does not visit it. The assertions
  are the contract's statements; the test documents the behaviour rather than claiming containment.
- C9, FIFO direct (new, real filesystem, production flags): `mkfifo root/pipe` with no writer;
  `file_slice pipe` returns `not a file` within 500 ms; `workspace_outline` lists `o pipe`;
  `text_search` counts it under `other` and never opens it.
- C10, file-to-FIFO replacement (new): hold after `statat` reports a regular file; replace the file
  with a FIFO; resume: the open returns at once, `fstat` reports a FIFO, result `not a file` within
  500 ms; the same with a file replaced by a directory (`not a file`) and a directory replaced by a
  file (`not a directory`).
- C11, socket (new): a bound Unix socket in the root: `file_slice sock` is `not a file`; if the
  fixture forces the race, the open fails and the result is still `not a file`.
- C12, unknown entry type (new): the fixture reports `Unknown` for an entry; the resolver falls back
  to `statat` and the outline kinds are correct.

## 4. Blocking work: ownership, accounting, supervision (F01, F03)

### 4.1 Why not `spawn_blocking`

Tokio documents that tasks spawned with `spawn_blocking` cannot be aborted, that a started closure
keeps running after the awaiting task is dropped, and that runtime shutdown waits indefinitely for
started blocking tasks unless `shutdown_timeout` abandons them, in which case they keep running. A
handler that returns `-32603` at its deadline would leave its read running, retire its request, and
free an admission permit for the next request to start another; the thread limit is "very large by
default". The plan uses no `spawn_blocking`.

### 4.2 Worker pool and resource accounting

- Four worker threads (`WORKERS`) created at startup when `--root` is given, owned by
  `workspace::worker`, each holding a borrowed root descriptor. Without `--root` no thread exists.
- Eight job slots (`JOB_SLOTS`), a `tokio::sync::Semaphore(8)`. The handler acquires a slot before
  enqueueing; the permit moves into the job and is released by the worker only when the job has
  finished and dropped every descriptor and buffer, never when the handler returns. Slots equal
  admitted requests, so a handler waits only when earlier jobs have outlived their handlers.
- A job carries its kind and validated arguments (at most 4,096 bytes), its deadline (the admitting
  handler's processing deadline, admission + 5 s, fixed at enqueue), a cancellation flag, a
  `oneshot` reply channel, and its permit. The queue is a `sync_channel(8)` and cannot block the
  handler because a permit is held before sending.
- Steps. Every system call is a step: `statat`, `openat`, `fstat`, `fcntl_getfl`, `fcntl_setfl`,
  one `read` of at most 65,536 bytes, one directory batch of at most 256 entries, `close`. Between
  steps the worker checks the cancellation flag and the deadline and stops with
  `stop_reason: time_budget`, or silently when cancelled. CPU work between steps is bounded by the
  table below.
- Handler protocol: validate arguments; acquire a slot within `SLOT_WAIT` = 2 s, else tool error
  `busy`; enqueue; await the reply inside the existing `run_with_deadline`. On deadline or client
  cancellation the handler sets the job's cancellation flag, returns `-32603 processing deadline
  exceeded` (deadline) or lets the adapter suppress the output (cancellation), and the job becomes
  orphaned: it stops at its next step boundary, releases its resources and permit, and its reply is
  dropped.
- Accounting published in `Counts` and diagnostics: `jobs_queued`, `jobs_running`,
  `jobs_orphaned`, `job_slots_available`, `job_descriptors_open`. Bounds: queued plus running at
  most 8; orphaned at most 8 (orphans hold slots); events `job.started kind=`, `job.finished
  stop_reason= steps=`, `job.orphaned`, `job.orphan_ended`, `job.busy` carry no paths.

Resource table, per worker unless stated, fixed at compile time:

| Resource | Bound | Accounting |
|---|---|---|
| descriptors per job, excluding the shared root | 2: the directory being walked or listed, plus one of the next component, the `Dir` iterator's descriptor, or the open file | `job_descriptors_open` |
| libc `DIR` buffer | one while an iterator is open; the iterator is dropped before any child is opened | counted with the iterator |
| read buffer (slice) | 65,536 bytes, one per worker, reused | fixed |
| file buffer (search) | 1,048,576 bytes, one per worker, reused; files over it are skipped `too_large` | fixed |
| result builder | 16,384 bytes per worker (12,288-byte cap plus working slack) | fixed |
| directory listing under sort | at most 5,000 entries per directory (`dir_entry_limit`), each a name of at most 255 bytes plus 16 bytes of type and size, about 1.36 MB | `dirs_truncated` counts directories with more entries, of which only the first 5,000 read were considered |
| path arena of queued directories | outline at most 5,000 nodes, search at most 4,096 nodes (`max_dirs`); each node a parent index, a type, and a name, at most 272 bytes; about 1.36 MB and 1.11 MB | `stop_reason: raw_entry_limit` or `max_dirs` |
| per-job argument storage | 4,096 bytes for each queued job, 8 jobs | fixed |
| CPU between steps | sorting at most 5,000 names; splitting and matching at most 1 MiB; rebuilding at most 16 KiB of result | covered by `JOB_GRACE` |

Totals: at most about 3.7 MiB per worker and 15 MiB for four workers, plus 32 KiB of queued
arguments. Memory is allocated once per worker at startup and reused; no allocation scales with the
tree beyond these bounds.

### 4.3 Supervision: step marker and job marker

Constants added to `limits`: `WORKERS` 4, `JOB_SLOTS` 8, `STEP` 2 s, `JOB_GRACE` 2.5 s,
`SLOT_WAIT` 2 s. Each worker owns two transaction markers in the supervisor's shared state (the
marker array grows from four to twelve entries, the snapshot and `evaluate` loop with it):

- the step marker, armed with `now + STEP` immediately before every step and cleared immediately
  after it returns;
- the job marker, armed at job start with `job.deadline + JOB_GRACE` and cleared when the job has
  released its permit. `job.deadline` is fixed at enqueue and unchanged by cancellation.

The supervisor latches `Cause::WorkerStall` (new variant, exit code 4, `fatal cause=worker_stall`)
when either marker is exceeded. Consequences: no step runs longer than `STEP` undetected; no job
lives longer than `PROCESSING + JOB_GRACE` = 7.5 s after admission undetected, so an orphan holds its
slot at most until then; a handler waiting for a slot waits at most `SLOT_WAIT`; every handler path
stays inside the 5 s processing budget. `JOB_GRACE` is one full step plus 0.5 s for the bounded CPU
work of 4.2.

Detection versus termination (the M0 mechanism, unchanged). The supervisor thread polls every
`limits::TICK` = 100 ms and reads only atomics. Detection: on the first poll at or after a marker's
deadline it latches the cause, emits `event=fatal cause=worker_stall code=4`, starts the 3 s
shutdown clock if none is running, and cancels the runtime's cancellation token (`latch_fatal`).
Termination: `main`'s serve loop observes the latched cause and exits with the cause's code without
joining threads; the shutdown clock is the backstop if `main` does not. Detection latency is at most
one tick; termination follows within the test allowance below. A shutdown clock already started by
EOF is not restarted by a later latch.

### 4.4 Shutdown

`close` waits, as in M0, until deliveries are acknowledged, and additionally until
`jobs_queued + jobs_running == 0`, or until a fatal cause, or until the 3 s shutdown budget, where
the supervisor forces exit 1 (`shutdown trigger=eof code=1`). Exit 0 requires zero requests, zero
deliveries, and zero jobs. Workers are never joined; `main` still ends with `std::process::exit`, so
the runtime's blocking-pool wait cannot occur. A step or job marker that expires during the shutdown
window is latched first and wins with exit 4 (S5).

### 4.5 Verification with production constants

Constants: PROCESSING 5 s from admission, STEP 2 s, JOB_GRACE 2.5 s, SLOT_WAIT 2 s, SHUTDOWN 3 s,
TICK 100 ms, ADMISSION 10 s. t0 is the admission of the request under test (from the `admitted`
diagnostic). Hold (a) is inside a step with the step marker armed; hold (b) is at the checkpoint
after a step with both markers clear. Observed times must fall in [deadline, deadline + TICK +
300 ms]; each test asserts the stderr cause and code, the exit code or response, and the counts.

| Test | Setup and timeline | Expected winner |
|---|---|---|
| S1 worker stall | hold (a) in the first `read`; step marker deadline t0+2.0 | exit 4 `worker_stall` in [2.0, 2.4] s; no response frame; handler (5.0) and admission (10.0) deadlines never reached |
| S2 handler deadline and orphan | hold (b) after the first step; at t0+5.0 the handler returns `-32603 processing deadline exceeded` (delivered in [5.0, 5.4] s), the request retires, `jobs_orphaned` 1, `job_slots_available` 7; release at t0+6.0 | no fatal; `job.orphan_ended` within 300 ms of release; counts restore to 0, 8, and `job_descriptors_open` 0; a following `connection_probe` is answered; EOF then exit 0 |
| S3 orphan past the grace | as S2, never released; job marker deadline t0+7.5 | exit 4 `worker_stall` in [7.5, 7.9] s via the job marker; the step marker is clear and the admission deadline was cleared at 5.0 |
| S4 orderly abandonment | hold (b); EOF at t0+0.5 starts the shutdown clock, deadline t0+3.5 | exit 1 `shutdown trigger=eof code=1` in [3.5, 3.9] s; handler deadline 5.0 and job marker 7.5 never reached; no step marker armed |
| S5 stall during shutdown | hold (a) in the first `read`; EOF at t0+0.5 (shutdown deadline 3.5); step marker deadline t0+2.0 | exit 4 `worker_stall` in [2.0, 2.4] s; the latch cancels the runtime and `main` exits before the shutdown deadline; the clock started by EOF is not restarted |
| S6 queued work completes | eight slices with a 50 ms fixture delay per step, then EOF at once | all eight responses delivered; exit 0 before the shutdown deadline |
| S7 busy | eight `file_slice` requests held (b); `notifications/cancelled` for all eight at t0+0.2 (handlers return at once, outputs suppressed, requests retire, eight orphans hold all slots); a ninth request at t0+0.3 waits SLOT_WAIT | `busy` in [2.3, 2.7] s; release at t0+2.7; counts restore; `job_slots_available` never negative at any event; EOF then exit 0; job markers (7.5) never reached |
| S8 descriptor baseline | `lsof -p` count before and after S2 and S7, as in M0 | equal to baseline |

Wire tests against the release binary confirm a 1 MiB file slice and a 5,000-entry outline complete
within budget without a `worker_stall`, and that the `fatal` event precedes exit in S1, S3, and S5.

## 5. Shared policy and frozen contracts (F03)

### 5.1 Shared policy, applied by every tool through one module

- Path arguments: section 3.1 rules; relative only; `""` means the root where a directory is
  expected and is `invalid path` where a file is expected.
- Hidden: a component whose name starts with `.`. Outline and search skip hidden entries unless
  `include_hidden: true`; `file_slice` reads a hidden file when it is not denied.
- Excluded directories, fixed list: `.git`, `target`, `node_modules`, `.venv`, `__pycache__`.
  Outline lists them with the `excluded` marker and does not descend; search does not descend;
  `file_slice` may read inside them. Exclusion governs traversal, denial governs content.
- Denied names, fixed list, matched against the final component: `.env`, `.env.*`, `*.pem`,
  `*.key`, `*.p12`, `*.pfx`, `id_rsa`, `id_rsa.*`, `id_ed25519`, `id_ed25519.*`, `id_ecdsa`,
  `id_ecdsa.*`, `*.kdbx`, `.netrc`, `.npmrc`, `.pypirc`, `credentials`, `credentials.*`,
  `*.keychain`, `*.keychain-db`. Outline lists such entries with the `denied` marker and no size;
  search counts them as skipped; `file_slice` returns `denied path` without opening. Per D2 these
  lists are documented guards, not a guarantee that no secret is readable.
- Symlinks: never followed anywhere. Outline lists `l` without a target; search counts them as
  skipped; `file_slice` on a symlink returns `invalid path`.
- Special files (FIFOs, sockets, devices, unknown types): `o` in outline, `other` in search,
  `not a file` in `file_slice`; never opened (3.1).
- Binary and text: a file is `binary` when its first 8,192 bytes contain a NUL byte; otherwise
  text. Invalid UTF-8 sequences are replaced by U+FFFD and `lossy: true` is set.
- Lines: split on `\n`; a trailing `\r` is removed and `line_endings` reports `lf`, `crlf`,
  `mixed`, or `none`; a final segment without `\n` is a line if non-empty; an empty file has zero
  lines. Cutting of long lines follows 5.6.
- Names in text blocks: UTF-8 with lossy replacement; bytes below 0x20 and 0x7F rendered as
  `\xNN`; a backslash rendered `\\`.
- Representation: `content[0]` is one `text` block carrying the compact text form; the structured
  content carries metadata only and never file text, match text, entry names, or the arguments
  (the client holds its own arguments, and not echoing them keeps the fit guarantee of 5.6
  independent of argument length); the output schema describes the metadata object. Codex displays
  both representations, so duplicating content would double the tokens.
- Caps, both apply: the serialized `result` object (`content`, `structuredContent`, `isError`) is at
  most 12,288 bytes, computed exactly before publication by the procedure of 5.6; the
  16,384-byte outbound frame bound remains the hard limit and cannot be reached because the
  envelope adds at most 320 bytes. `max_bytes` on `file_slice` bounds the UTF-8 bytes of returned
  line text after `\r` removal and cutting, excluding prefixes and suffixes.
- Ordering: each directory is read completely (up to `dir_entry_limit`) and its entries sorted by
  name bytes; outline lists breadth-first by depth; search visits directories in that order and
  reports matches in line order within each file.

### 5.2 `workspace_outline`

Input schema: object, `additionalProperties: false`; `path` string, max 1,024, default `""`;
`depth` integer 1 to 3, default 2; `max_entries` integer 1 to 200, default 100; `include_hidden`
boolean, default false.

Output schema (structured, all required): `entries_listed` integer 0 to 200 (lines in the text
block after capping); `cap_omitted` integer; `truncated` boolean; `stop_reason` one of `complete`,
`max_entries`, `raw_entry_limit`, `time_budget`; scan counters `dirs_visited`, `dirs_truncated`,
`excluded_dirs`, `denied_entries`, `symlinks`, `hidden_skipped`, `other_skipped`.

Text grammar: one line per entry, `<kind> <path>[ <bytes>][ excluded|denied]`, where `<kind>` is
`d`, `f`, `l`, or `o`, `<path>` is root-relative with `/`, and `<bytes>` appears only for regular
files that are not denied. Traversal: the start directory is re-resolved from the root and read
completely (at most `dir_entry_limit` 5,000 entries, the rest unread and counted in
`dirs_truncated`); entries are sorted and listed; subdirectories at depth below `depth` are queued
in the path arena and visited in order, each re-resolved from the root. At most 5,000 raw entries
are read in total (`raw_entry_limit`); listing stops at `max_entries`. Counters cover only entries
actually read. An empty directory yields an empty text block and `entries_listed: 0`.

### 5.3 `file_slice`

Input schema: `path` string 1 to 1,024, required; `start_line` integer 1 to 10,000,000, default 1;
`max_lines` integer 1 to 400, default 120; `max_bytes` integer 256 to 12,288, default 8,192.

Output schema (structured): required `start_line`, `returned_lines` (0 to 400), `at_eof` boolean,
`last_line_partial` boolean, `file_bytes`, `bytes_read`, `encoding` (`utf-8` or `binary`), `lossy`,
`line_endings` (`lf`, `crlf`, `mixed`, `none`), `lines_cut`, `cap_omitted`, `truncated`,
`stop_reason` one of `complete`, `max_lines`, `max_bytes`, `time_budget`, `line_too_long`,
`binary`, `start_past_eof`; optional `end_line` (present when `returned_lines > 0`, equal to
`start_line + returned_lines - 1` after capping) and `total_lines` (present when `file_bytes` is at
most 1,048,576 and the count completed within the budget).

Text grammar: `<line_number><TAB><text>` per line, with the cutting and trailer rules of 5.6.
Zero-line texts: `[binary file, <file_bytes> bytes]` with `stop_reason: binary`; `[no lines:
start_line <n> is past EOF]` with `stop_reason: start_past_eof` and `at_eof: true`; `[no lines: time
budget exhausted before line <n>]` with `stop_reason: time_budget` when the deadline falls while
skipping to `start_line`. Semantics: lines before `start_line` are skipped by counting while
streaming 64 KiB steps; the first returned line is always returned, cut to `max_bytes` if
necessary; a later line that would exceed `max_bytes` ends the slice with `stop_reason: max_bytes`;
`at_eof` is true when the last returned line is the file's last line, is complete, and nothing was
removed by the cap.

### 5.4 `text_search`

Input schema: `query` string 1 to 128 characters without control characters, required; `path`
string, max 1,024, default `""` (directory or file); `case_sensitive` boolean, default false
(insensitive matching folds ASCII letters only); `max_matches` integer 1 to 100, default 40;
`context_lines` integer 0 to 2, default 0; `include_hidden` boolean, default false.

Output schema (structured, all required): `query_chars`; `matches_found` 0 to 100 (collected during
the scan); `matches_reported` (match lines in the text block after capping); `cap_omitted` (match
and context lines removed by the cap); `files_scanned`; `files_matched`; `files_skipped` object with
integer fields `binary`, `too_large`, `denied`, `symlink`, `hidden`, `excluded`, `unreadable`,
`other`; `dirs_visited`; `dirs_truncated`; `bytes_scanned`; `truncated`; `stop_reason` one of
`complete`, `max_matches`, `max_files`, `max_dirs`, `max_entries`, `scan_budget`, `time_budget`.

Text grammar: for each file with at least one reported line, a header `== <path>` followed by
`<line>:<text>` for matches and `<line>-<text>` for context lines, with `--` between non-adjacent
groups within a file. The path appears once per file. Bounds: `max_files` 2,000 files scanned;
files over 1,048,576 bytes skipped as `too_large`; `scan_budget` 64 MiB; `time_budget` 2 s;
`max_dirs` 4,096 directories visited or queued; `max_entries` 20,000 raw directory entries read;
`dir_entry_limit` 5,000 per directory. Each file is read completely into the worker's file buffer
in at most 16 steps and matched line by line in memory, so the end of every match line is known.

### 5.5 Errors

Tool errors (`isError: true`, one fixed message): `missing path`, `invalid path`, `not found`,
`not readable` (D3), `not a file`, `not a directory`, `denied path`, `invalid argument`,
`unexpected argument`, `arguments too large`, `busy`. The M0 argument bound of 4,096 bytes stays.
The handler deadline remains `-32603 processing deadline exceeded`. No message carries content,
names, or host paths.

### 5.6 Cutting, partial lines, and result capping

Cutting. A returned line longer than 400 characters (slice) or 200 characters (search) is cut at a
character boundary and suffixed ` [+N chars]`, where N is the number of characters not shown. N is
exact only when the end of the line was reached. In `text_search` it always is, because the whole
file is in memory. In `file_slice`, after cutting a line the worker continues reading that line to
count N for at most 1,048,576 further bytes and within the deadline; if the line has not ended by
then, the suffix is ` [+? chars]`, `last_line_partial` is true, and the slice stops with
`stop_reason: line_too_long` (look-ahead exhausted) or `time_budget` (deadline). Only the last
returned line can be partial. A line cut by `max_bytes` follows the same suffix rule.

Result capping. The unit of removal is one text line: a slice line, an outline entry, or a search
match or context line; a search file header whose lines have all been removed is removed with them
and is not counted. Procedure: serialize the complete `result`; while it exceeds 12,288 bytes,
remove the last unit, increment `cap_omitted`, recompute the report metadata, rebuild the trailer,
and serialize again. Report metadata recomputed from the retained text: slice `returned_lines`,
`end_line`, `lines_cut`, `last_line_partial` (false once the partial line is removed), and `at_eof`
(true only when the file's last line is retained, complete, and `cap_omitted` is zero); outline
`entries_listed`; search `matches_reported`. Scan metadata never changes with capping: `file_bytes`,
`bytes_read`, `total_lines`, `line_endings`, `lossy`, `encoding`, every counter, `files_scanned`,
`files_matched`, `bytes_scanned`, `matches_found`, `dirs_visited`, and `stop_reason`, which names
why collection stopped.

`truncated` is true when `stop_reason` is not `complete`, or `cap_omitted` is nonzero, or
`last_line_partial` is true. Trailer: when `truncated` is true the last text line is
`[truncated: <stop_reason>]`, `[truncated: result_cap, <cap_omitted> <units> omitted]`, or
`[truncated: <stop_reason>; result_cap, <cap_omitted> <units> omitted]`, with `<units>` `lines` or
`entries`. The zero-line texts of 5.3 are not followed by a trailer.

Fit guarantee. Metadata is fixed keys and integers of at most ten digits, at most 800 bytes
serialized; the trailer is at most 64 bytes. One unit, every byte JSON-escaped in the worst case:
an outline entry or search header is at most 2 + 5 × 1,024 + 12 + 9 bytes (a 1,024-byte path whose
every byte renders as `\xNN` and then escapes to five bytes), under 5,200; a slice line at most
8 + 1 + 6 × 400 + 14 bytes, under 2,500; a search line at most 8 + 1 + 6 × 200 + 14, under 1,300.
A result with zero units is therefore under 1,000 bytes and a result with one unit under 6,200, so
the first unit always fits and the loop ends after at most one iteration per unit, each a
serialization of at most 16,384 bytes.

Tests (F03): a slice whose 400-character cut line continues past the look-ahead limit
(`[+? chars]`, `last_line_partial`, `line_too_long`); a cut line whose end is reached (`[+N chars]`
exact); the fixture deadline falling inside a long line (`time_budget`, partial); capping that
removes the previously last line of a file (`at_eof` flips to false, `end_line` and
`returned_lines` recomputed, `cap_omitted` 1); capping that removes a whole search file group (header
removed, `matches_reported` below `matches_found`, `files_matched` unchanged); a result at exactly
12,288 bytes (no capping) and one byte over (one unit removed); a 5,001-entry directory
(`dirs_truncated` 1); a tree of 4,097 directories (`max_dirs`); 20,001 raw entries (`max_entries`);
depth 3 with 5,000 raw entries (`raw_entry_limit`), each asserting the memory bounds of 4.2 through
the fixture's allocation counters and `job_descriptors_open` at most 2.

## 6. Compatibility

Same binary, transport, registry, supervisor, frame and admission bounds, and exit codes, plus exit
4 for `worker_stall`. Without `--root` the wire behaviour is byte-identical to `0dc00896…` except
the version string, verified by the unchanged M0 `tools/list` golden and the full M0 suites. With
`--root`, a second `tools/list` golden lists four tools with `readOnlyHint`, `idempotentHint`,
`openWorldHint: false`, cache hints `ttlMs: 0` for slice and search, `ttlMs: 1000` for outline,
`cacheScope: private`, and `resultType: complete`. Both eras list and call the tools. The probe's
behaviour and bounds are unchanged in both configurations. Through tunnel-client the one-lifecycle
rule and id reuse are unaffected; results stay far below the hosted 10 MiB limit.

## 7. Evidence, retention, and correlation for Milestone 1 proofs (F05)

### 7.1 Retention classes

Temporary raw traces, written to the run's temporary directory, consumed by the extractor, hashed
(SHA-256 recorded in the retained run record), and deleted before the run is archived: Claude Code
stream-json transcripts and raw hook payloads; Codex `--json` streams and stderr; fixture copies;
the harness's raw answer text. They may contain canaries, fixture paths, account identifiers, and
file content; that is expected and is why they are not retained. Raw traces never leave the run
directory and never enter `docs/`.

Retained sanitized artifacts, under `docs/evidence/m1/`: relay manifests and records (content-free
by construction); per-run extracts holding usage fields, the ordered tool-call sequence as `tool`,
`args_sha256`, `args_bytes`, `result_sha256`, `result_bytes`, `is_error`, `error_code`, checker
verdicts and scores without answer text, the answer's SHA-256, compliance verdicts with reasons,
timings, client and model versions from client output, raw-trace hashes, and the correlation
status; aggregate reports. Every retained artifact must be free of `CANARY_DIR_7f3a`,
`CANARY_CONTENT_9c1e`, the fixture's absolute path, and account identifiers, checked by the same
scan M0 used before commit. The canary requirement applies to retained artifacts only; a canary in a
raw trace after a tool legitimately returned it is the positive control that the extractor removes
content.

### 7.2 Relay and hook changes (in scope)

The relay's known-tool set becomes `connection_probe`, `workspace_outline`, `file_slice`,
`text_search`; `tools/call` request records gain `args_sha256` (16 hex characters of SHA-256 over
the canonical form of `arguments`, 7.3) and `args_bytes`; response records gain `result_sha256`
(16 hex characters over the UTF-8 bytes of `result.content[0].text`), `result_bytes`, and
`is_error`; a JSON-RPC error response records `error_code` with `result_sha256: null`. No argument,
path, name, or content is recorded. The hook extractor gains `tool_input_sha256` and
`tool_response_sha256` computed the same way. A relay test feeds synthetic frames containing both
canaries and asserts their absence from records and manifests.

### 7.3 Canonical form and digest vectors

Canonical JSON for `args_sha256`, implemented once in Rust (relay) and once in Python (harness):
objects with keys sorted by Unicode code point sequence, no whitespace, integers in decimal (any
non-integer number makes the digest `null` with `args_canonical: false`, and correlation for that
call falls back to order), `true`, `false`, `null` literal, strings with `"` and `\` escaped as
`\"` and `\\`, U+0008, U+000C, U+000A, U+000D, U+0009 as `\b`, `\f`, `\n`, `\r`, `\t`, other
code points below U+0020 as `\u00xx` lowercase, and everything else, including U+007F and non-ASCII,
as raw UTF-8. The digest is SHA-256 over the UTF-8 bytes, first 16 hex characters. Vectors, which
both implementations must reproduce in tests (computed with Python 3 on 2026-11-06):

| Arguments (canonical form) | Bytes | `args_sha256` |
|---|---|---|
| `{}` | 2 | `44136fa355b3678a` |
| `{"path":""}` | 11 | `518341a7f82a2945` |
| `{"max_lines":60,"path":"src/main.rs","start_line":1}` (same digest from either input key order) | 52 | `addece581e7e5c6f` |
| `{"case_sensitive":false,"context_lines":0,"query":"outbound_too_large"}` | 71 | `22d8cd22e833ca09` |
| `{"max_bytes":256,"path":"docs/été/日本.md"}` | 47 | `78bbd623f9a8a6d3` |
| `{"max_matches":100,"query":"tab\there \"quoted\" back\\slash \u0001ctl"}` (the query holds a tab, two quotes, a backslash, and U+0001) | 72 | `22d9e5e171640ea5` |
| `{"depth":3,"include_hidden":true,"max_entries":200,"path":"a"}` | 62 | `096afd0e9fb660cb` |
| `{"A":"upper sorts first","b":{"a":"x","z":[1,2,{"y":null}]},"é":"non-ascii key"}` | 81 | `77c4c3da126e8539` |

`result_sha256` is over the exact UTF-8 bytes of the text block: the empty text is
`e3b0c44298fc1c14`; the text `d .\nf README.md 1024\n` (with a real newline after `.` and at the
end) is `023dfd9f7a5c3f0d`.

### 7.4 Correlation rules

For each run the harness forms the ordered list of the client's tool calls (Claude Code
`tool_use.input` with its `tool_result`; Codex `mcp_tool_call.arguments` with its result) and the
ordered list of the relay's `tools/call` requests with their responses. Key: `(tool, args_sha256)`.
The k-th client call with key K matches the k-th relay request with key K. A match is complete when
the response digests also agree, or when the relay response is a JSON-RPC error and the client
records an error for that call (the client's error text is its own and is not compared). Tool
errors (`isError: true`) carry text and are compared by digest like any result. Repeated identical
calls with identical results are matched by order within their key and reported as "corroborated by
count and digest, not individually distinguishable". If the per-key counts differ, or any digest
disagrees, or a relay request has no client counterpart, the run's `correlation` is `incomplete`
with the reason; such a run is never described as corroborated, though its usage still counts in
section 8. `connection_probe` keeps nonce correlation.

Tests: a successful outline whose text contains `CANARY_DIR_7f3a` run through the complete pipeline
(the raw trace contains it, every retained artifact does not, correlation complete); a run with only
error calls (a tool error from `missing path`, a `-32602` from a malformed argument) correlates
completely with null result digests where applicable; three identical outline calls match by order;
a synthetic count mismatch and a synthetic digest mismatch produce `incomplete` and a report that
never says corroborated; the eight vectors of 7.3 reproduce in both implementations.

## 8. Comparison methodology with attribution controls (F04)

Fixture: this repository at a pinned commit copied to a fresh temporary directory per run (the
canary entries added), no `CLAUDE.md` or `AGENTS.md`.

Tasks, fixed wording in a hashed `tasks.json`: T1 describe the top-level layout; T2 state the value
and file of `MAX_OUTBOUND_FRAME_BYTES`; T3 reproduce lines 1 to 60 of `src/main.rs`; T4 list the
test functions in `tests/wire_errors.rs`; T5 list every document mentioning `outbound_too_large`;
T6 reproduce the budgets table from `docs/transport.md`.

Fixed configuration: Claude Code with `--model` pinned to one identifier, `--strict-mcp-config`,
a `--settings` file containing only the evidence hook, `--max-turns 12`, arm-specific
`--allowedTools` and `--disallowedTools`; Codex with `--ignore-user-config --ephemeral`, `-c
model=<id>`, `-c model_reasoning_effort=<fixed>`, read-only sandbox, `approval_policy=never`,
`-C <fixture>`. Client versions and model identifiers are recorded from the clients' own output.

Arms per client: A native tools only, token-saver not configured; B token-saver only, with Claude
Code's `Read`, `Grep`, `Glob`, and `Bash` disallowed, and for Codex an instruction plus an audit;
C both available.

Session isolation: one process per run, fresh fixture copy, fresh relay output directory, fresh
nonce, no resume or session reuse.

Attempts per cell (client, task, arm): exactly seven, fixed in advance. Attempt 0 is a warm-up,
reported separately and excluded from every statistic; attempts 1 to 6 are measured whatever their
outcome. There are no retries and no top-up runs: a failed, timed-out, noncompliant, or incorrect
attempt stays in the cell with its usage. Arm order rotates per task (ABC, BCA, CAB, …) so warm-up
and cache effects are counterbalanced across arms. Cache fields are reported, never folded into a
single number.

Usage normalization, without double counting: Claude Code uses `result.usage` only: uncached input
= `input_tokens`, cache write = `cache_creation_input_tokens`, cache read =
`cache_read_input_tokens`, output = `output_tokens`. Codex sums `turn.completed.usage` over the
run: total input = `input_tokens`, cached = `cached_input_tokens`, uncached = total minus cached,
output = `output_tokens` with `reasoning_output_tokens` reported as its component. An attempt that
ends without a usage record (timeout, crash) is reported as `usage unavailable`, counted in the
denominators, and makes the cell's failure-inclusive totals lower bounds, labelled as such. Raw
fields and formulas are stored with every run.

Outcome classes per attempt: completed (exit 0 within 180 s), compliant (arm A shows no relay
traffic; arm B on Claude Code shows no `tool_use` other than token-saver tools and `ToolSearch`;
arm B on Codex shows no `command_execution` or non-MCP tool item; arm C unconstrained), and
correctness `correct`, `partial`, `incorrect`, or `no answer` from the task checker. A
noncompliant arm B attempt is a failure of the arm's control, reported as such with its usage; no
attempt is called "token-saver only" until its audit passes.

Reported per cell, denominators always shown:

- counts: completed, compliant, correct, partial, out of 6;
- failure-inclusive usage: sum and median of each usage field over all six measured attempts;
- expenditure per correct answer: the sum over all six attempts of uncached input, cache write, and
  output tokens, divided by the number of correct attempts; "no correct answers" when zero;
- success-conditioned medians over correct and compliant attempts, labelled "success-conditioned,
  k of 6";
- bytes and tool-call counts per attempt.

Pre-registered claim rule: a saving for a task and client is stated only when arm B's correct
count is at least arm A's, arm B's expenditure per correct answer is lower, and the
success-conditioned medians point the same way; otherwise the report states "no saving
established" with all numbers. Nothing is aggregated into a single percentage.

Correctness per task: T1 set overlap of the real top-level names, precision and recall at least
0.9; T2 exact value and exact file path; T3 exact normalized reproduction: the largest fenced block
(or the whole answer if none) with trailing whitespace stripped per line, line endings normalized,
and leading and trailing blank lines dropped must equal the 60 lines; anything else is `partial`
with the number of matching lines reported separately and counted as incorrect; T4 set equality
with the parsed test names; T5 set equality with the grep result; T6 exact row-wise match of budget
names and values, otherwise `partial`. A negative fixture, an answer containing the expected
substrings but wrong, must fail each checker; a partial-copy fixture must be classed `partial`.

Byte metrics, named by observation point, none called model-visible: `relay_response_frame_bytes`
(the JSON-RPC response frame on the server's stdout, envelope and both representations),
`relay_result_text_bytes` (the text block), and `client_recorded_result_bytes` (Claude Code: the
`tool_result` content block in the stream-json transcript; Codex: the `mcp_tool_call` item's
`result` in the `--json` stream). The payload Codex sends to the model is not in its `--json`
output: at tag `rust-v0.167.0`, `core/src/tools/context.rs` prefixes the result with a
`Wall time: <seconds> seconds` and `Output:` header and applies `truncate_function_output_payload`
under a token budget taken from the prepared call's `output_token_limit` or the model's truncation
policy (`handlers/mcp.rs`). Client-reported usage, not bytes, is the cost measure, and it already
includes whatever the client presented. The offline tokenizer estimate (tiktoken, pinned,
`o200k_base`, labelled an approximation whose correspondence to the current models is unverified)
applies to `client_recorded_result_bytes` only.

ChatGPT: functional calls of each tool through the tunnel by the operator, `relay_*` bytes from
the relay, correctness by the operator's export; token counts Unsupported.

Harness tests (F04): synthetic trials where arm A is correct 6 of 6 at 1,000 tokens and arm B is
correct 3 of 6 at 800 tokens with 1,200-token failures, so success-conditioned medians favour B
while expenditure per correct answer is 2,000 against 1,000 and the report must say "no saving
established"; a partial copy of `src/main.rs` classed `partial`; a `usage unavailable` attempt
producing a labelled lower bound; a Codex result larger than its presentation would keep, reported
only under the named observation points.

## 9. Tests and acceptance criteria

Tests beyond sections 3.5, 4.5, 5.6, 7.4, and 8: unit tests for every policy rule and every bound at
its limit and one over; wire tests for both goldens, each tool in both eras, each error, multibyte
text and escaping at the cap, long paths, a skipped-only tree, an empty directory, an empty file,
CRLF and mixed endings, lossy UTF-8, a binary file, denied and excluded entries, and a search that
hits each `stop_reason`; fixture canaries for content and names at every diagnostics level; schema
goldens and validation.

| ID | Criterion | Evidence and method | Party |
|---|---|---|---|
| M1-01 | Workload confirmed, task set fixed | reviewer decision recorded; hashed task file | reviewer, agent |
| M1-02 | Contracts implemented as frozen in section 5 | schema goldens, wire tests, 5.6 tests | agent |
| M1-03 | Authority contract of 3.2 demonstrated, including move-out and special files | C1 to C12 | agent |
| M1-04 | Blocking work bounded through completion with the stated timelines | S1 to S8 with production constants | agent |
| M1-05 | M0 unchanged without `--root` | M0 suites green, golden unchanged | agent |
| M1-06 | Each tool invoked from Claude Code and Codex and correlated | 7.4 correlation complete for every call including errors; retained artifacts canary-free | agent |
| M1-07 | Comparison run with the controls of section 8 | `docs/evidence/m1/` run records, failure-inclusive report, claim rule applied | agent |
| M1-08 | ChatGPT functional tests and payload bytes | operator run with relay; export | operator |
| M1-09 | Documentation and ADR-0002 | `docs/` | agent |
| M1-10 | Diff, outputs, truthful records, no credentials | commit review | agent, reviewer |

## 10. Dependencies

Runtime: `rustix =1.1.6` (features `std`, `alloc`, `fs`; default features off) and its transitive
`bitflags` and `libc` on macOS; no regex engine; nothing else. Dev: unchanged, plus fixture
helpers, `mkfifo` and Unix sockets for C9 to C11. Harness: Python 3 standard library and an
optional pinned `tiktoken` in an operator-created virtual environment. Clients as installed on the
run day, versions recorded from their own output. The dependency delta, binary size delta,
`cargo deny` and `cargo audit` results, and notices update are reported with the implementation.

## 11. Risks, assumptions, unresolved requirements

Risks: secrets inside a root reach the model and, through ChatGPT, OpenAI (the lists of 5.1 are
guards per D2; operator guidance is the control: project roots only, never a home directory,
ChatGPT roots only with content the operator would upload); a directory held by a running job and
moved out of the root stays readable to that job within the windows of 3.1 (documented in 3.2,
tested in C8, optional hardening D4); models over-fetching (caps and arm C); both representations
inflating results (content appears once, in text); model nondeterminism (six measured attempts,
failure-inclusive reporting); Claude Code runtime selection varying per session (both eras
supported and recorded); a stalled filesystem ending the process with exit 4 (documented, the M0
posture for stalls); the owned resolver carrying correctness risk (3.5 tests).

Assumptions to confirm: the workload; one root per process; ChatGPT file access wanted at all;
Codex's audited instruction accepted as the arm B control.

Unresolved: U1, the user's spending priority (planning continues around (a) per the review); D4,
whether the optional identity walk of 3.3 is required (proposed: not included).

## 12. Exact implementation scope requested for approval

In scope: `src/workspace/` with `confine.rs` (root descriptor, classification, resolver,
`FileSystem` trait with the step checkpoint), `worker.rs` (pool, slots, steps, markers,
accounting), `policy.rs`, `render.rs` (cutting, capping, trailer, fit), `outline.rs`, `slice.rs`,
`search.rs`, and `mod.rs` (tool registration beside the probe); `--root` parsing in
`src/main.rs`; eight worker markers and `Cause::WorkerStall` in `src/supervisor.rs`; the constants
of 4.3 and 5.4 in `src/lib.rs`; diagnostics events of 4.2; the tests of 3.5, 4.5, 5.6, 7.4, 8,
and 9; relay and hook-extractor changes of 7.2 and 7.3; `tools/token-compare/` (runner, retention
and extraction, compliance audit, usage normalizer, checkers, correlation, report); documentation
(`docs/tools.md`, `docs/configuration.md`, `docs/transport.md` worker section, `docs/security.md`
T18 to T23 including stalled filesystems and move-out, `docs/adr/0002-retained-directory-authority.md`,
`docs/benchmark-methodology.md`, client docs for `--root`); `docs/evidence/m1/` after the runs.
Estimated 2,600 to 3,200 lines of Rust including tests and about 700 lines of Python.

Out of scope: writes, command execution, regex search, symlink following, multiple roots, indexing,
persistent state, ChatGPT token measurement, the identity walk unless D4 requires it, and any
token-saving figure before the comparison has run.

Decisions requested: U1 (user), D4 (optional hardening, proposed not included).
