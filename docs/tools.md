# Tools

The server exposes exactly one tool.

## connection_probe

| Element | Contract |
|---|---|
| Name | `connection_probe` |
| Title | Connection probe |
| Input | Required `nonce`: 1 to 64 ASCII letters or digits; additional properties rejected |
| Output | Exactly `nonce`, `instance_id`, `server_version`, `status`; all required; additional properties rejected |
| `instance_id` | 32 lowercase hexadecimal characters, random per process, stable within a process |
| `status` | The constant `"ok"` |
| Annotations | `readOnlyHint: true`, `destructiveHint: false`, `idempotentHint: true`, `openWorldHint: false` |
| Representation | `structuredContent` plus one text block holding the same compact JSON |

Input schema:

```json
{"type":"object","additionalProperties":false,"required":["nonce"],
 "properties":{"nonce":{"type":"string","minLength":1,"maxLength":64,"pattern":"^[A-Za-z0-9]+$"}}}
```

Output schema:

```json
{"type":"object","additionalProperties":false,
 "required":["nonce","instance_id","server_version","status"],
 "properties":{"nonce":{"type":"string","minLength":1,"maxLength":64,"pattern":"^[A-Za-z0-9]+$"},
               "instance_id":{"type":"string","pattern":"^[0-9a-f]{32}$"},
               "server_version":{"type":"string","maxLength":16},
               "status":{"const":"ok"}}}
```

The schemas are hand-written constants in `src/probe.rs`; `tests/fixtures/tool.golden.json` pins
the full tool definition. Idempotence means safe repeated execution; a restart changes `instance_id`.

### Example

Request:

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"connection_probe","arguments":{"nonce":"abc123"}}}
```

Result (legacy session; a `2026-07-28` session adds `"resultType":"complete"`):

```json
{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"{\"instance_id\":\"0b709e3bab9944caa356f04a1df2c3cb\",\"nonce\":\"abc123\",\"server_version\":\"0.1.0\",\"status\":\"ok\"}"}],"structuredContent":{"instance_id":"0b709e3bab9944caa356f04a1df2c3cb","nonce":"abc123","server_version":"0.1.0","status":"ok"},"isError":false}}
```

### Tool errors

Valid invocations with bad arguments return a compact tool result with `isError: true` and one of
these fixed texts: `missing nonce`, `invalid nonce`, `unexpected argument`, `arguments too large`.

## Error contract

| Condition | Response |
|---|---|
| Frame is not valid JSON | `-32700`, id omitted |
| Not an object, batch array, `jsonrpc` not `"2.0"`, id null or non-integer number or object, `method` not a string, `params` not an object, no method and not a response | `-32600`, id included only when it is a usable string or integer |
| Id over 256 serialized bytes | `-32600`, id omitted, never echoed |
| Any frame whose usable id is already owned by an unretired request or pending error | connection terminated, exit 3, no second response |
| Unrecognized method with absent or object params | `-32601` |
| Recognized method whose params fail the typed shape | `-32602` |
| Unknown tool | `-32602`, message `tool not found` |
| Valid call with missing, invalid, or extra argument | tool result, `isError: true` |
| Unsupported `_meta` protocol version, on any method, at any lifecycle stage | `-32022` with `data.supported` and `data.requested`; enforced by the adapter before the SDK sees the request |
| Any error frame, adapter-generated or SDK-produced, whose serialized form exceeds 16,384 bytes (for example a `requested` version too long to echo) | never written; `outbound.oversized` then `fatal cause=outbound_too_large`, exit 3 |
| Modern request missing required `_meta` | `-32602` |
| Cooperative deadline reached | `-32603`, message `processing deadline exceeded` |
| Internal failure | `-32603`, message `internal error` |
| Valid notification, client response, cancellation | no response |
| `ping` without modern metadata, before or after `initialize` | empty result |
| `ping` carrying `2026-07-28` metadata, at any lifecycle stage | `-32601`; enforced by the adapter, since `ping` does not exist in that revision |

Recognized methods for the `-32602` rule: `initialize`, `ping`, `server/discover`, `tools/list`,
`tools/call`, `prompts/list`, `prompts/get`, `resources/list`, `resources/read`,
`resources/templates/list`, `resources/subscribe`, `resources/unsubscribe`, `completion/complete`,
`logging/setLevel`, `subscriptions/listen`.

## Server identity and instructions

`serverInfo` is `token-saver` with the Cargo package version. `instructions` is a single paragraph
under 512 characters describing the probe. Capabilities declare `tools` only.
