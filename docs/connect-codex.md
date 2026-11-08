# Codex CLI

Codex CLI 0.167.0 at `/opt/homebrew/bin/codex` on this host; no `mcp_servers` configured, no
`/etc/codex/requirements.toml`, no `com.openai.codex` preference domain, `CODEX_HOME` unset
(verified read-only on 2026-11-05). Its MCP client is rmcp `=3.5.1` at a pinned git revision.

## Facts from the pinned source

- Default protocol mode is `Legacy`: `initialize` at `2025-06-18`, then `notifications/initialized`.
  The pinned client accepts whatever `protocolVersion` the server returns, so the server's
  `2025-11-25` answer is expected to be accepted. The feature `mcp_2026_07_28` is under development
  and off here.
- The child receives only `HOME`, `LOGNAME`, `PATH`, `SHELL`, `USER`, `__CF_USER_TEXT_ENCODING`,
  `LANG`, `LC_ALL`, `TERM`, `TMPDIR`, `TZ`, plus configured `env` and `env_vars`; cwd is the configured
  `cwd` or the session directory; stderr is logged line by line at `info`.
- Startup timeout 10 s, tool timeout 60 s, parallel tool calls off by default.
- `default_tools_approval_mode` accepts `auto`, `prompt`, `writes`, `approve`.

## Automated verification, nothing persisted

```
N=$(openssl rand -hex 8)
RUST_LOG=info codex exec --ignore-user-config --ephemeral --skip-git-repo-check -C "$TMP/wd" --json \
  -c 'mcp_servers.token-saver.command="/abs/evidence-relay"' \
  -c "mcp_servers.token-saver.args=[\"--expect-nonce\",\"$N\",\"--out\",\"/abs/evidence/codex-auto\",\"--\",\"/abs/token-saver\",\"serve\",\"--diag\",\"info\"]" \
  "Call the connection_probe tool on the token-saver MCP server with nonce $N and report the instance_id it returns."
```

Evidence: the `item.completed` line whose item has `type: mcp_tool_call`, `server`, `tool`,
`arguments`, `result`, `status`; the relay record with `nonce_match: true` and `instance_id`; the
`initialize` version observed by the relay; Codex's stderr lines carrying the server's `startup`
diagnostic; the binary hash.

## Interactive verification, operator

Same `-c` overrides in an interactive `codex` session. For per-call denial add
`-c 'mcp_servers.token-saver.default_tools_approval_mode="prompt"'` and deny at the prompt; the
relay file must show no `tools/call` and be complete. Disable with
`-c 'mcp_servers.token-saver.enabled=false'` in a fresh session: the tool is absent and no child is
spawned. Exiting the session ends the child; a new session yields a new `instance_id`.

## Workspace Reading Pack (Milestone 1)

Add `--root /abs/project` to the server command. Codex lists the tools under server `token-saver`
and records each call as an `mcp_tool_call` item with `server`, `tool`, `arguments`, and `result`.
Codex shows the model both representations of a tool result when both exist, so the text-only
results of the workspace tools (`docs/tools.md`) reach it once. Before the model sees a result,
Codex prefixes a wall-time header and applies its own truncation policy
(`core/src/tools/context.rs` at `rust-v0.167.0`); the harness therefore names its byte metrics by
observation point and never calls them model-visible.

Automated verification (`tools/token-compare/compare.py proof --client codex`): one `codex exec`
run with the relay, in a fresh fixture copy with `-c model="gpt-6.1-sol"`,
`-c model_reasoning_effort="low"`, `-c sandbox_mode="read-only"`, `-c approval_policy="never"`,
asked to call each tool once and to provoke two tool errors. Codex reports no model identifier in
`exec --json`; the record carries the pinned value and says so. The comparison's arm B relies on an
instruction sentence plus an audit of the items (any `command_execution` or non-MCP item, or any
call to another server, is noncompliant). Results: `docs/evidence/m1/proofs/`.

## Persistent installation and removal, operator

```
codex mcp add token-saver -- /abs/path/token-saver serve
codex mcp get token-saver
codex mcp remove token-saver
```

## Status

Automated verification: Passed on 2026-11-06 against binary `0dc00896…` (`docs/evidence/m0/codex-auto.*`). The `mcp_tool_call`
item names server `token-saver`, tool `connection_probe`, the nonce, and the result; the relay
records show `initialize` at `2025-06-18` answered with `2025-11-25` and accepted, then
`notifications/initialized`, `tools/list`, and `tools/call`; Codex's log carried the server's
`startup` line with the same `instance_id` (`codex-auto.log-extract.txt`). The run used
`approval_policy=never` and a read-only sandbox; the read-only tool ran without a prompt. `codex exec`
reads stdin when it is a pipe, so scripts must redirect `< /dev/null`, otherwise it waits
indefinitely before starting MCP servers.

Interactive allow and deny, disable, and the persistent install and removal are operator steps: Not
executed.
