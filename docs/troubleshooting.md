# Troubleshooting

| Symptom | Likely cause | Check |
|---|---|---|
| exit code 3 right after a large message | frame over 64 KiB, duplicate id, tenth concurrent request, or five undelivered error frames | stderr `event=fatal cause=...`; the cause names the rule |
| exit code 4 after about 5 s | the client stopped reading stdout, or a handler exceeded its budget | stderr `fatal cause=delivery_deadline` or `admission_deadline`; confirm the client drains stdout |
| exit code 4 after about 10 s with no response | work that ignored cancellation, or the runtime stopped dispatching | `fatal cause=admission_deadline` or `acceptance_deadline` |
| exit code 1 | orderly EOF or signal with work still undelivered at 3 s | stderr `shutdown trigger=eof code=1` |
| `-32602 request _meta is missing...` | a `2026-07-28` request without required metadata | add `io.modelcontextprotocol/protocolVersion` and `clientCapabilities` |
| `-32022` | unsupported `_meta` version | use one of `data.supported` |
| `initialize` answered with `2025-11-25` though another version was requested | fallback for older handshakes | expected; see compatibility |
| `ping` with `2026-07-28` metadata answered `-32601` | `ping` does not exist in that revision; the adapter rejects it at every lifecycle stage | use `server/discover` as the modern liveness probe, or legacy `ping` without `_meta` |
| `notification.dropped reason=no_credit` or `outstanding` | duplicate `initialized`, or sent before the initialize response was produced | expected bounded behaviour |
| tunnel-client returns HTTP 409 and `-32002` | ChatGPT sent a legacy call before `initialize` and `notifications/initialized` passed the helper | capture traffic; enable the helper's `initialized` shim only with evidence the caller omitted it |
| ChatGPT lists no tools | daemon not running, wrong tunnel id, or `/readyz` not 200 | `tunnel-client doctor --explain`, `/health/mcp` |
| Claude Code shows the server as failed | wrong absolute path or the binary is not executable | `claude mcp get token-saver`; run the binary by hand |
| Codex shows no tools | `-c` override parse error, or startup exceeded 10 s | `codex mcp get token-saver`; `RUST_LOG=info` shows "MCP server stderr" lines |
| `codex exec` never starts MCP servers from a script | stdin is an open pipe; Codex reads it as additional input | redirect `< /dev/null` |
| `claude -p` prints a stdin warning | same cause | redirect `< /dev/null` |

Where diagnostics go: Claude Code captures the server's stderr itself (destination verified during
bring-up and recorded in the evidence pack); Codex logs each stderr line at `info`; tunnel-client
forwards stderr to its own stderr. Run the binary by hand with `--diag debug` to see every event.
