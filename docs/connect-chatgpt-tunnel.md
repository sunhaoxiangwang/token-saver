# ChatGPT Web through the Secure MCP Tunnel

Status: the probe was performed on 2026-11-06 by the operator through tunnel-client 0.0.20 and is
corroborated by two independent sources in `docs/evidence/m0/`: the relay capture
(`chatgpt.relay.*`, complete, six records, nonce `39df3fb4db92370e`, instance `7932b647…`) and the
result the user reported from ChatGPT (`chatgpt-reported-result.json`), which carry the same nonce
and instance id; `chatgpt-verification.json` records the helper version and the server and relay
hashes, which equal the approved binaries. Still missing from this capture: ChatGPT's own tool-call
export, the helper's stderr carrying the server's `startup` line, and the Platform audit entries.
The disable, re-enable, daemon stop and restart, child kill, and removal steps have no evidence and
remain Not executed. Milestone 0 exposes exactly one tool, `connection_probe`; ChatGPT lists
nothing else, and a successful probe proves reachability, not token saving.

The helper is a long-lived daemon, not a one-shot launcher: every ChatGPT call to token-saver is
long-polled from OpenAI by the helper and forwarded to the stdio child it supervises. It must stay
running, with `/readyz` returning 200, for as long as ChatGPT is expected to call the server. If it
stops, ChatGPT calls fail until it is started again, and the restarted server has a new
`instance_id`.

Two helper versions are kept apart below. The facts were researched in the pinned source of
tunnel-client v0.0.19 (released 2026-10-30) while planning. The host now has v0.0.20 (released
2026-11-06; Homebrew tap `openai/tools`; git sha `c2144b09…`), installed since the 2026-11-05
preflight that found no helper, and verified read-only on 2026-11-06 together with a profile at
`~/.config/tunnel-client/token-saver.yaml`, which was not read. Profiles, runtime keys, raw helper
logs, and build artifacts stay outside the repository; the ignore rules cover `*.key` and
`tunnel-client*.yaml` as a guard.

## Facts from the pinned source and documentation, v0.0.19

- The helper long-polls `api.openai.com` over outbound HTTPS and forwards JSON-RPC verbatim to one
  supervised stdio child. It splits the command string with shell-style quoting and runs it without a
  shell; cwd and environment are inherited from the helper; the child's stderr goes to the helper's
  stderr.
- One request lifecycle at a time crosses the shared child, whatever the server's capacity.
- Legacy calls before `initialize` and `notifications/initialized` are answered locally with HTTP 409
  and JSON-RPC `-32002`. Requests carrying `_meta` protocol version pass the guard.
- `mcp.stdio_send_initialized_notification` defaults to false. Enable it only after relay evidence
  shows the caller omitted the notification; record the change and retest.
- `stdio_failure_policy: restart` restarts the child after an unexpected exit with backoff from
  100 ms to 5 s. Run only one active helper per tunnel id for stdio.
- Hosted limits: 600 requests per minute, 10 MiB per message, 120 s deadline, 1,000 pending.
- Health surfaces named in that research: `/health/mcp` and `/health?details=true`.

## Observations with the installed helper, v0.0.20 (read-only, 2026-11-06)

- `tunnel-client --version` reports `0.0.20+c2144b0904980bcbae2636cdca4f4167e9818e0b`.
- The help text confirms every command and flag the procedure uses: `init --sample
  sample_mcp_stdio_local --profile <name> --tunnel-id <id> --mcp-command "<cmd>"` (`--sample` is
  auto-selected as the stdio sample when `--mcp-command` is given), `--control-plane-api-key-ref`
  taking `env:VAR` or `file:/path` (default `env:CONTROL_PLANE_API_KEY`), `doctor --explain`, `run`,
  and a `health` command that probes `/healthz` and `/readyz` with an optional PID cross-check.
- `run` defaults: `--mcp.max-concurrent-requests 10`, `--mcp.stdio-failure-policy restart`,
  `--mcp.stdio-send-initialized-notification` off unless opted in. `GET /health` accepts
  `details=true`. The daemon also serves `/ui`. `/health/mcp` does not appear in the v0.0.20 help and
  must be confirmed on a live daemon before the evidence list relies on it.
- v0.0.20 bundles a `cloudflared` companion and a `codex` assistant surface; neither is used here.
- Release notes: v0.0.20 concerns OAuth forwarding across HTTPS origins and startup-probe logging and
  lists no stdio, failure-policy, or `initialized` changes; v0.0.19 introduced stdio child restart
  after an unexpected exit, the `exit` policy, session-state reset on recovery, and the rule that
  in-flight requests fail and are not replayed.
- Wire observation from the operator run (relay capture in `docs/evidence/m0/chatgpt.relay.*`):
  ChatGPT used the modern era, `server/discover` with `2026-07-28` metadata, then `tools/list`, then
  `tools/call`, each carrying `2026-07-28` metadata; no `initialize`. The helper started the child
  at daemon start and the first request arrived about 236 s later; the probe call came 87 s after
  `tools/list`; server round trips were 2 to 5 ms; the child ran 587 s and exited 0 on a clean EOF
  when the daemon stopped. `tools/list` and `tools/call` carried the same request id, which the
  server accepted because the first had retired before the second arrived: through the helper,
  request ids repeat across requests. The helper's launch quoting worked with the relay command as
  written in step 3.
- Not observed: ChatGPT's confirmation behaviour (no client export), the helper's stderr, and the
  Platform audit entries.

## Operator procedure

1. Platform: create the tunnel (Tunnels Read and Manage), associate the ChatGPT workspace, create a
   Restricted runtime key with Tunnels Read and Use. Record the audit log entry.
2. Host: `brew tap openai/tools && brew install openai/tools/tunnel-client`. Record the version
   (v0.0.20 is installed as of 2026-11-06).
3. Profile: `tunnel-client init --sample sample_mcp_stdio_local --profile token-saver --tunnel-id <id> --control-plane-api-key-ref file:/abs/0600/runtime.key --mcp-command "/abs/evidence-relay --expect-nonce <N> --out /abs/evidence/chatgpt -- /abs/token-saver serve --diag info"`
   for the evidence run, or the direct server command for installation. Set
   `mcp.max_concurrent_requests: 8` in the profile. Keep the profile and key file outside the
   repository.
4. `tunnel-client doctor --profile token-saver --explain`, then `tunnel-client run --profile token-saver`
   from a shell with a minimal environment. Confirm `/readyz` returns 200 (`tunnel-client health`)
   and keep the daemon running for the whole session.
5. ChatGPT Web: Plugins, add, Add custom MCP server, Connection: Tunnel, select the tunnel id,
   Authentication: No authentication, acknowledge the risk warning, Create as a plugin, install.
6. One conversation: `@token-saver`, ask for the probe with the fresh nonce. Record the confirmation
   behaviour exactly as observed; expand the tool call details and save the JSON.
7. Disable the tool on the plugin page; new conversation; record. Re-enable; new conversation; record.
8. Stop the daemon; new conversation; record. Restart; new `instance_id`. Kill the child; observe the
   restart policy.
9. Removal: uninstall the plugin, delete the tunnel (Tunnels Manage), revoke the runtime key, delete
   the profile and key file.

## Evidence

ChatGPT tool-call JSON, relay records with `nonce_match: true` and `instance_id`, the observed
`initialize` version or `_meta` version, `/readyz` and `/health?details=true` output (and
`/health/mcp` if it exists), the helper's stderr carrying the server's `startup` line, the binary
hash, the helper version, and the platform audit log entries. Per-call denial is Unsupported if
ChatGPT shows no confirmation for a read-only tool. Every item is labelled with the helper version.

## Workspace Reading Pack (Milestone 1), operator procedure

Passed for the functional calls and payload bytes: run on 2026-11-07 by the operator through
tunnel-client 0.0.20 against the approved binary `7c749250…` and relay `836c963b…` (the `verification` build of `docs/evidence/m1/artifact-index.json`),
with the relay in front of the server and `--root` pointing at a four-file demo directory outside the
repository (`README.md`, `docs/notes.md`, `src/main.rs`, and a placeholder `.env`). A new profile
`token-saver-m1` was created with `init --sample sample_mcp_stdio_local`, the runtime key by `file:`
reference and the relay command carrying `--root`; `doctor --explain` reported ok; `health --port 8080
--require-control-plane-poll` reported OK once the control-plane poll had succeeded, about 30 s after
start (`health` takes no `--profile` flag in v0.0.20). The operator was instructed to set
`mcp.max_concurrent_requests: 8` in the profile; whether the line was added was not confirmed (the
profile is not read here), and the run never had more than one request in flight. Evidence in
`docs/evidence/m1/chatgpt/`:

- `chatgpt.relay.manifest.json` and `chatgpt.relay.records.jsonl`: `complete: true`, `reason:
  orderly`, 6 frames in, 6 out, 12 records, zero drops and late frames, child exit 0 on clean EOF in
  both directions after 1,084 s. ChatGPT sent `server/discover` (`2026-07-28` metadata), `tools/list`
  (5,249-byte response listing the four tools), then `workspace_outline {}`, `text_search
  {"query":"needle"}`, `file_slice {"path":"src/main.rs","start_line":1,"max_lines":10}`, and the
  erroneous `file_slice {"path":".env"}` (`is_error: true`, `denied path`). Server round trips were 1
  to 4 ms. The manifests' argv paths are replaced by placeholders; the originals' hashes are in the
  verification file. `chatgpt.relay.run-1.manifest.json` is the first daemon start, stopped after
  46 s before any traffic.
- `chatgpt-reproduction.json`: the four calls replayed locally against the same root with the same
  binary; every `args_sha256` and `result_sha256` equals the relay record.
- `chatgpt-reported-result.txt` and `.json`: the ChatGPT reply saved by the operator; the three
  successful outputs are byte-identical to the server text, and the error reached the user as an
  `INVALID_ARGUMENT` exception text quoting `denied path`. The UI offered no per-call details to
  export, so this is a `user_reported_tool_result`, weaker than a client export.
- `chatgpt.helper-stderr-extract.txt`: the server's `startup` and `workspace.root` lines from the
  helper's stderr (missing in Milestone 0) and the six forwarded commands, reduced; the shutdown line
  was not retained because the tee pipeline ended with the Ctrl-C signal.
- `chatgpt-verification.json`: helper version, hashes, payload bytes, timeline, and what is missing.

Observations: as in Milestone 0, `tools/list` and every `tools/call` carried the same request id
(the helper forwards rpc id 0) and the server accepted each because the previous one had retired. The
first ChatGPT request arrived 378 s after daemon start; the four calls spanned 7.3 s. The relay's
`--expect-nonce` came from the init template; the Milestone 1 procedure calls no `connection_probe`,
so `nonce_match` is false on every record and correlation rests on the digests. The connector page
showed the permission setting `Allow low-risk`; whether ChatGPT prompted per call was not recorded,
so per-call denial remains Unsupported. Token counts through ChatGPT are Unsupported; payload bytes
come from the relay records (request frames 4,207 bytes and response frames 1,617 bytes over the four
calls, 1,135 bytes of result text).

Still missing: ChatGPT's own tool-call export, the Platform audit entries, and the doctor and health
outputs (pasted during the run, not retained). Not executed for Milestone 1: disable, re-enable,
daemon stop and restart, child kill, and removal.

To repeat it: add `--root /abs/project` to the helper's `--mcp-command` (quote the command), using
only a directory whose content you would upload to ChatGPT, then from ChatGPT call
`workspace_outline`, `text_search`, and `file_slice` once each and one erroneous call, with the relay
in the command so the capture records `args_sha256` and `result_sha256` per call.
