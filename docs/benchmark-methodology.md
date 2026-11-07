# Benchmark methodology

Milestone 0 reports baselines only. No token savings, throughput guarantee, or comparison claim is
made. Numbers are reported separately for the server alone, the tunnel helper alone, and the
complete client path, and only after they were measured. Results live in `docs/evidence/`.

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
