# Limitations of Milestone 0

- One tool, no resources or prompts, no HTTP transport, no authorization framework. stdio only.
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
- Raw client logs may contain account identifiers (Codex logs the account email at `info`); they are summarized and deleted under the evidence policy.
- Verified on macOS arm64 only. Linux builds are expected to work but were not run.
- Interactive client behaviours (permission prompts, denials, ChatGPT confirmation) and the ChatGPT
  path itself require operator steps and are recorded only when executed.
