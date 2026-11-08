# Configuration

## Server flags

```
token-saver serve [--diag off|error|info|debug] [--root /abs/dir]
token-saver --version
```

`serve` is the only operating mode. `--diag` selects the diagnostics level written to stderr; the
default is `info`. `off` still emits the single `startup` line. `--root` enables the Workspace
Reading Pack over one directory: the path must be absolute and name a readable directory that is
not a symlink; it is opened once at startup, under the 3 s startup budget (a stalled lookup is a
`startup_stall`, exit 4), and is never printed. A refused root (missing, a symlink, a file,
unreadable) emits the `root.refused` diagnostic and exits with code 2.
Without `--root` the server is byte-identical to Milestone 0 on the wire except the version string.
There is no configuration file, and no environment variable is read. Unknown flags exit with code 2
and print usage on stderr.

## Fixed limits

| Limit | Value |
|---|---|
| Inbound frame including newline | 65,536 bytes; exceeding it closes the connection, exit 3 |
| Serialized request id | 256 bytes; longer ids are rejected with `-32600` and never echoed |
| Tool `arguments` object, serialized | 4,096 bytes |
| Serialized probe `result` object | 1,024 bytes |
| Admitted concurrent requests | 8 |
| Unretired requests, admitted plus pending | 9; a tenth is `admission_overflow`, exit 3 |
| Outstanding adapter-generated error frames | 4; a fifth while all are undelivered is `error_flood`, exit 3 |
| Serialized outbound frame | 16,384 bytes |
| Cooperative processing deadline, from admission | 5 s |
| Delivery deadline, from commitment | 5 s |
| Unresolved admission, from admission | 10 s |
| Acceptance to admission, from acceptance | 10 s |
| Registry transaction | 1 s |
| Startup | 3 s |
| Shutdown, orderly or fatal | 3 s |
| Supervisor interval | 100 ms, detection latency, not a guarantee |
| Diagnostics | 256 queued events, 64 per second, then dropped and counted |
| Supported uptime | 800 days, after which the server shuts down with exit 4 |

Workspace Reading Pack (`--root` only):

| Limit | Value |
|---|---|
| Worker threads | 4 |
| Job slots | 8; a handler waits at most 2 s for one, then answers the tool error `busy` |
| Step budget, one blocking system call, from the step's start | 2 s; exceeding it is `worker_stall`, exit 4 |
| Job grace after the handler's processing deadline | 2.5 s; a job still holding its slot at admission + 7.5 s is `worker_stall`, exit 4 |
| Serialized workspace `result` object | 12,288 bytes, enforced by bounded construction |
| Path argument and generated path | 1,024 bytes, 64 components of at most 255 bytes |
| Read step | 65,536 bytes |
| Entries read per directory | 5,000; more makes the directory `truncated` |
| Outline raw entries per job | 5,000 |
| Search | 2,000 files, 1 MiB per file, 64 MiB scanned, 2 s scanning, 4,096 directories, 20,000 raw entries |
| Binary detection | NUL within the first 8,192 bytes |
| Line cutting | 400 characters (slice), 200 characters (search); 1 MiB look-ahead to count a cut line's remainder |
| `total_lines` | reported for files of at most 1 MiB |
| Per-worker memory | 64 KiB read buffer, 1 MiB file buffer, 12 KiB text buffer, 16 KiB result buffer, a 5,000-entry listing, a 5,000-node path arena |
| Descriptors per job | at most 2 besides the shared root descriptor |

Advertised protocol versions: `2025-11-25` and `2026-07-28`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Orderly exit after stdin EOF, SIGINT, or SIGTERM with every accepted request retired and every response acknowledged by the writer |
| 1 | Orderly trigger, but accepted work or undelivered output was still outstanding at the 3 s shutdown budget and was abandoned |
| 2 | Usage error |
| 3 | Protocol violation: oversized frame, duplicate in-flight id, admission overflow, error flood, outbound frame too large |
| 4 | Timing, I/O, or invariant failure: delivery deadline, unresolved admission, acceptance deadline, registry or startup stall, worker stall (a workspace step over 2 s or a job over its grace), read or write error, uptime limit |
| 5 | The SDK reported an initialization failure other than EOF |

The first fatal cause wins. An orderly trigger never overwrites a fatal cause and never prevents a
later one from being recorded.

## Diagnostics

One line per event on stderr: `ts_ms=<ms since start> event=<name> k=v ...`. Events carry fixed,
typed fields only: never a request id, a client method string outside the known set, a reason text,
or any payload. The SDK's own log events are disabled by installing no tracing subscriber, so they
cannot reach stderr at any level. `startup` carries `version`, `instance_id`, and
`supported_versions`; `response.sent` carries `handler_ms` and `admit_to_delivery_ms` separately.
With `--root`: `workspace.root` carries `workers` and `slots` (never the path); `job.started`,
`job.finished` (`kind`, `stop_reason`, `steps`, `ms`), `job.orphaned`, `job.orphan_ended`, and
`job.busy` carry the job kind only. No event carries a path, a name, or content.

## Client configuration

Every client launches the absolute path of the binary with the single argument `serve`. These steps
are performed by the operator; see the connection documents for verification and evidence steps.

Claude Code, local scope, stored in the operator's `~/.claude.json`, not in the repository:

```
claude mcp add --scope local --transport stdio token-saver -- /abs/path/token-saver serve
claude mcp add --scope local --transport stdio token-saver -- /abs/path/token-saver serve --root /abs/project
```

Codex CLI, stored in `~/.codex/config.toml`:

```
codex mcp add token-saver -- /abs/path/token-saver serve
codex mcp add token-saver -- /abs/path/token-saver serve --root /abs/project
```

The second form of each enables the Workspace Reading Pack over `/abs/project`. Declare project
roots only, never a home directory, and for ChatGPT only roots whose content you would upload: the
exclusion and denial lists are guards, not a guarantee.

tunnel-client profile for ChatGPT Web, stored under `~/.config/tunnel-client/`:

```
tunnel-client init --sample sample_mcp_stdio_local --profile token-saver \
  --tunnel-id tunnel_... --mcp-command "/abs/path/token-saver serve"
```

Then edit the profile so `control_plane.api_key` is a `file:` reference to a key file with mode
0600, and set `mcp.max_concurrent_requests: 8`. Quote the command if the path contains spaces;
tunnel-client splits the string on whitespace with shell-style quotes and runs it without a shell.
