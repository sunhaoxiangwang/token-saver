# Claude Code

Claude Code 2.1.319 on this host (the version recorded in the `init` line of every captured `stream-json` run; an earlier host description said 2.1.318). No managed MCP policy files exist and the user settings contain
no MCP allow or deny lists (verified read-only on 2026-11-05).

## Automated verification, nothing persisted

Uses a temporary MCP configuration and the evidence relay so that the client's own tool event, the
relay record, and the server's startup line can be matched on one fresh nonce.

```
N=$(openssl rand -hex 8)
cat > "$TMP/mcp.json" <<JSON
{"mcpServers":{"token-saver":{"type":"stdio","command":"/abs/evidence-relay",
 "args":["--expect-nonce","$N","--out","/abs/evidence/claude-auto","--","/abs/token-saver","serve","--diag","info"]}}}
JSON
cat > "$TMP/settings.json" <<JSON
{"hooks":{"PostToolUse":[{"matcher":"mcp__token-saver__connection_probe",
 "hooks":[{"type":"command","command":"python3 -I /abs/tools/evidence-relay/hook_extract.py /abs/evidence/claude-auto/hook.jsonl $N"}]}]}}
JSON
claude -p --strict-mcp-config --mcp-config "$TMP/mcp.json" --settings "$TMP/settings.json" \
  --allowedTools "mcp__token-saver__connection_probe" --output-format stream-json --verbose --max-turns 3 \
  "Use the token-saver MCP server's connection_probe tool with nonce $N and report the instance_id it returns."
```

Evidence: the `tool_use` block naming `mcp__token-saver__connection_probe` with the nonce, the
`tool_result` content, the hook extract with `nonce_match: true`, the relay record with
`nonce_match: true` and the `instance_id`, and the binary hash. The relay's manifest must be
`complete: true`.

## Interactive verification, operator

Start a terminal session with the same temporary configuration and no `--allowedTools`:

```
claude --strict-mcp-config --mcp-config "$TMP/mcp.json" --settings "$TMP/settings.json"
```

1. Allow: ask for the probe with a fresh nonce; allow at the permission prompt; record the prompt,
   the transcript, and the relay record.
2. Deny: in a fresh session, deny at the prompt; the relay file must show `tools/list` but no
   `tools/call` and be `complete: true`.
3. Disable: `/mcp`, disable the server, exit; in a fresh session the tool is absent and no child is
   spawned (no new relay run). Re-enable; in a fresh session the tool is present again.
4. Disconnect and reconnect: `/mcp` Reconnect yields a new `instance_id`.

## Persistent installation, operator

```
claude mcp add --scope local --transport stdio token-saver -- /abs/path/token-saver serve
claude mcp get token-saver
```

Local scope writes the operator's `~/.claude.json`, not the repository. Claude Code stops the server
with SIGINT, then SIGTERM after 100 ms, then SIGKILL after 400 ms more; the server exits on SIGINT.

## Removal

```
claude mcp remove token-saver --scope local
claude mcp list
```

## Runtimes

Claude Code picks the v2 runtime on sessions that fetch feature flags and asks stdio servers for
`2026-07-28`; `MCP_SDK_GENERATION=v1` or `MCP_PROTOCOL_NEGOTIATION=legacy` selects the `initialize`
handshake. The relay record of each run shows which era was used.

## Status

Automated verification: Passed on 2026-11-06, two runs against binary `0dc00896…`
(`docs/evidence/m0/claude-auto.run-1.*`, `claude-auto.run-2.*`). In each run the client's `tool_use`
block, the hook extract (`nonce_match: true`; `duration_ms` 9 and 14), and the relay records agree on
one nonce and on `instance_id`, and the relay manifest is `complete: true`. The two runs of the same
CLI version and configuration selected different eras: run 1 used the `initialize` handshake at
`2025-11-25` followed by `notifications/initialized`, `tools/list`, and `tools/call`; run 2 used
`server/discover`, `tools/list`, and `tools/call`, each carrying `2026-07-28` metadata. The relay
record of a run is therefore the only reliable statement of the era it used.
Note: `claude -p` warns when stdin is a pipe with no data; redirect `< /dev/null` in scripts.

Interactive allow, deny, disable, reconnect, and the persistent install and removal are operator
steps: Not executed.
