# Compatibility

## Protocol versions

| Version | Status |
|---|---|
| `2026-07-28` | advertised and tested: `server/discover`, per-request `_meta`, `resultType`, cache hints |
| `2025-11-25` | advertised and tested: `initialize` handshake echoed |
| `2025-06-18`, `2025-03-26`, `2024-11-05` | not advertised; an `initialize` at these versions is answered with `2025-11-25`, tested |
| any other `_meta` version | `-32022` listing the two advertised versions, tested |

The SDK (`rmcp 3.9.0`) knows five versions. token-saver restricts the advertised set to two; the
restriction is tested, not assumed. Expanding it requires client evidence that a required client
needs an earlier revision.

## Clients

| Client | Expected negotiation | Evidence status |
|---|---|---|
| Claude Code 2.1.319, v2 runtime | asks for `2026-07-28` on stdio | Passed, automated run 2 of 2026-11-06 against binary `0dc00896…`: `server/discover`, then `tools/list` and `tools/call`, all carrying `2026-07-28` metadata; see docs/evidence/m0 |
| Claude Code 2.1.319, v1 runtime | `initialize` at `2025-11-25` | Passed, observed unforced in automated run 1 of 2026-11-06 with the same CLI version and configuration: `initialize` at `2025-11-25` accepted, then `notifications/initialized`, `tools/list`, `tools/call`; forcing it with `MCP_SDK_GENERATION=v1` was not executed |
| Codex CLI 0.167.0, default `Legacy` mode | `initialize` at `2025-06-18`; pinned client accepts the `2025-11-25` answer | Passed, automated run 2026-11-06 against binary `0dc00896…`: `initialize` at `2025-06-18` answered with `2025-11-25`, accepted, `notifications/initialized`, `tools/list`, `tools/call`; see docs/evidence/m0 |
| Codex CLI 0.167.0 with `mcp_2026_07_28` | `Auto` lifecycle, `server/discover` first | not executed; feature flag off here |
| ChatGPT Web via tunnel-client (facts from the 0.0.19 source; 0.0.20 installed on the host 2026-11-06) | documented as `initialize` then `tools/list`; version unknown until captured | not executed; requires operator account steps; the helper must stay running for every call |

## SDK behaviours recorded during verification

- `server/discover` is a bootstrap probe and selects no lifecycle; the first non-discover modern
  request selects the inline lifecycle. The SDK's pre-initialization loop would answer `ping` and
  skip version validation for it; the adapter validates `_meta` versions and rejects modern `ping`
  itself, so the wire behaviour does not depend on lifecycle state.
- A recognized method whose params fail the typed shape is parsed by the SDK as a custom request;
  the adapter maps that to `-32602` per the error contract.
- Cache hints `ttlMs: 0` and `cacheScope: private` are added to `2026-07-28` list and discover
  results; legacy results omit `resultType`.
- A repeated `initialize` in a session is answered again.
