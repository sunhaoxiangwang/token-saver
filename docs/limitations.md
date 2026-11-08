# Limitations

## Milestone 0 server

- Without `--root`, one tool; with `--root`, four. No resources or prompts, no HTTP transport, no authorization framework. stdio only.
- Concurrency: 8 admitted requests plus 1 pending. A tenth unretired request closes the connection
  with exit 3. Clients that issue more than nine concurrent requests to this server cannot use it.
- Frames over 65,536 bytes, ids over 256 serialized bytes, and arguments over 4,096 bytes are
  rejected. The probe result is limited to 1,024 bytes.
- Advertised protocol versions are `2025-11-25` and `2026-07-28`. Older handshakes receive
  `2025-11-25`; clients that cannot accept it disconnect.
- `notifications/cancelled` is applied by the reader as soon as the frame is read, whatever the
  state of other requests: a queued request is dropped at dispatch with no response, an admitted
  request has its handler cancelled and its output suppressed, and a committed request is recorded as
  a late cancellation and still delivered. Resolution of cancelled admitted work is bounded by the 5 s
  processing and 10 s admission budgets.
- All notifications other than `initialized` and `cancelled`, and all client responses, are dropped.
- Diagnostics are rate limited to 64 events per second and may be dropped; diagnostic-based
  assertions are inconclusive when the `diag.dropped` counter is nonzero.
- SDK internal bookkeeping (completed send-task records, proxy channel) is measured, not enforced.
- Supported uptime is 800 days; the process shuts down with exit 4 when reached.
- `ping` carrying `2026-07-28` metadata is answered `-32601` at every lifecycle stage, since the
  method does not exist in that revision; `ping` without modern metadata is answered with an empty
  result before and after `initialize`. Neither depends on which requests preceded it.
- Through tunnel-client, request lifecycles are serialized to one at a time by the helper regardless
  of the server's capacity, and the helper daemon must stay running for every ChatGPT call.
- Open compatibility issue: on 2026-11-08 ChatGPT did not show the results of four of six tool calls
  the server answered successfully ("no visible result text"); the cause, on the ChatGPT side, is not
  established. See `docs/compatibility.md`.
- The ChatGPT app page offers no control that disables the app without deleting it.
- Raw client logs may contain account identifiers (Codex logs the account email at `info`); they are summarized and deleted under the evidence policy.
- Verified on macOS arm64 only. Linux builds are expected to work but were not run.
- Interactive client behaviours (permission prompts, denials, ChatGPT confirmation) and the ChatGPT
  path itself require operator steps and are recorded only when executed.

## Workspace Reading Pack (`--root`)

- One root per process, declared at startup; no runtime change, no multiple roots, no MCP `roots`
  negotiation.
- Read-only: no writes, no command execution, no network, no indexing, no state between jobs.
- Symlinks are never followed; FIFOs, sockets, and devices are never read; hidden entries are
  skipped unless asked for; `.git`, `target`, `node_modules`, `.venv`, `__pycache__` are never
  entered; the denied-name list blocks common credential files. These lists are guards, not a
  guarantee that no secret inside a root is readable.
- Retained-directory authority (`docs/adr/0002-retained-directory-authority.md`): a directory held
  by a running job and renamed out of the root stays readable to that job until the job ends; content
  moved or hard-linked into the root is inside.
- Search is a literal substring match with ASCII-only case folding, no regular expressions, no
  word boundaries, at most 2,000 files of at most 1 MiB each, 64 MiB and 2 s of scanning per call.
- Results are capped at 12,288 bytes; lines over 400 (slice) or 200 (search) characters are cut;
  a line whose end is not reached within 1 MiB of look-ahead or within the deadline reports
  `[+? chars]`.
- Directories with more than 5,000 entries are read partially (`dirs_truncated`), and entries
  whose root-relative path exceeds 1,024 bytes or 64 components are counted `too_deep` and skipped.
- Eight job slots across four workers; a ninth concurrent job waits up to 2 s, then `busy`. A step
  that stalls for 2 s, or a job that outlives its handler by 2.5 s, ends the process with exit 4.
- Token savings are not claimed by the server or its documentation; the comparison harness reports
  what it measured (`docs/evidence/m1/`).
