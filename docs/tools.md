# Tools

Without `--root` the server exposes exactly one tool, `connection_probe`, and its wire behaviour is
that of Milestone 0. With `--root <absolute directory>` it also exposes the Workspace Reading Pack:
`workspace_outline`, `file_slice`, and `text_search`, three read-only tools served by an owned
worker pool over the root directory descriptor (see the second half of this document and
`docs/adr/0002-retained-directory-authority.md`).

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

## Workspace Reading Pack (`--root`)

Every workspace tool returns exactly one text block and nothing else: the content lines in a
compact form, an optional `[truncated: <stop_reason>]` trailer, and a final metadata line that
starts with `# ` and holds `key=value` pairs (nested keys joined with `.`). No `structuredContent`
is sent and no output schema is advertised. Reason, observed on 2026-11-07 with Claude Code 2.1.319:
when a result carries `structuredContent`, that client hands the model the structured JSON and drops
the text block, so content placed in the text block never reaches the model; Codex shows both
representations, which would duplicate content. Text-only results reach every client once. The
metadata never carries file text, match text, entry names, or arguments. The serialized `result`
object is at most 12,288 bytes, enforced by bounded construction (a text unit is admitted only when
its JSON-escaped length fits the remaining budget, the metadata line's maximum cost is reserved, and
the final serialization is checked exactly; an invariant failure is the tool error `internal error`,
never an oversized frame). The 16,384-byte outbound frame bound remains the hard limit. Input
schemas and the metadata schemas (which describe the parsed metadata line, for tests and this
document) are hand-written constants in `src/workspace/mod.rs`;
`tests/fixtures/tools.root.golden.json` pins the three definitions.

### Shared policy

| Rule | Behaviour |
|---|---|
| Path arguments | UTF-8, 1 to 1,024 bytes, no NUL, no leading `/`, 1 to 64 components of 1 to 255 bytes, none `.` or `..`; `""` means the root where a directory is expected and is `invalid path` where a file is expected. Anything else is `invalid path` before any I/O. |
| Generated paths | A traversal entry whose root-relative path exceeds the argument limits is counted `too_deep` and is neither listed, descended, nor scanned. |
| Hidden | A component starting with `.`. Outline and search skip hidden entries unless `include_hidden` is true; `file_slice` reads a hidden file that is not denied. |
| Excluded directories | `.git`, `target`, `node_modules`, `.venv`, `__pycache__`: listed with the `excluded` marker, never entered by outline or search; `file_slice` may read inside them. |
| Denied names | `.env`, `.env.*`, `*.pem`, `*.key`, `*.p12`, `*.pfx`, `id_rsa`, `id_rsa.*`, `id_ed25519`, `id_ed25519.*`, `id_ecdsa`, `id_ecdsa.*`, `*.kdbx`, `.netrc`, `.npmrc`, `.pypirc`, `credentials`, `credentials.*`, `*.keychain`, `*.keychain-db`, matched on the final component with `*` standing for any string including the empty one (so `.env.` and `credentials.` are denied): outline lists them with the `denied` marker and no size, search skips them, `file_slice` returns `denied path` without opening. One implementation serves all three tools. These lists are documented guards, not a guarantee that no secret inside a root is readable. |
| Symlinks | Never followed. Outline lists `l path`, search counts them as skipped, `file_slice` on a symlink is `invalid path`. |
| Special files | FIFOs, sockets, devices, and unknown types are classified before any open and rejected: `o path` in outline, `other` in search, `not a file` in `file_slice`. An object that becomes a special file between classification and open is opened non-blocking, identified by `fstat`, and closed unread. |
| Binary | A file whose first 8,192 bytes contain NUL. |
| Text | Lines split on `\n`; a trailing `\r` is removed and `line_endings` reports `lf`, `crlf`, `mixed`, or `none`; invalid UTF-8 is replaced by U+FFFD with `lossy: true`. |
| Cutting | A returned line over 400 characters (slice) or 200 characters (search) is cut at a character boundary with the suffix ` [+N chars]`; when the end of the line was not reached the suffix is ` [+? chars]` and `last_line_partial` is true. |
| Names in text | UTF-8 with lossy replacement; bytes below 0x20 and 0x7F rendered `\xNN`; a backslash rendered `\\`. |
| Ordering | Each directory is read completely (at most 5,000 entries; more makes it `truncated`) and sorted by name bytes; outline lists breadth-first by depth; search visits directories in that order. |
| Trailer | When `truncated` is true the last text line is `[truncated: <stop_reason>]`. |

### workspace_outline

| Element | Contract |
|---|---|
| Input | `path` (string, max 1,024, default `""`), `depth` (1 to 3, default 2), `max_entries` (1 to 200, default 100), `include_hidden` (boolean, default false); additional properties rejected |
| Text | one line per entry: `<kind> <path>[ <bytes>][ excluded\|denied]`, kind `d`, `f`, `l`, or `o`; `<bytes>` only for regular files that are not denied |
| Metadata line | `entries_listed`, `truncated`, `stop_reason` (`complete`, `max_entries`, `raw_entry_limit`, `result_cap`, `time_budget`), `dirs_visited`, `dirs_truncated`, `dirs_unreadable`, `excluded_dirs`, `denied_entries`, `symlinks`, `hidden_skipped`, `other_skipped`, `too_deep` |
| Bounds | at most 5,000 raw entries read per job; each directory re-resolved from the root when visited; an empty directory yields an empty text block |

### file_slice

| Element | Contract |
|---|---|
| Input | `path` (required, 1 to 1,024), `start_line` (1 to 10,000,000, default 1), `max_lines` (1 to 400, default 120), `max_bytes` (256 to 12,288, default 8,192, bounds the UTF-8 bytes of returned line text) |
| Text | `<line><TAB><text>` per line; zero-line texts `[binary file, <bytes> bytes]`, `[no lines: start_line <n> is past EOF]`, `[no lines: time budget exhausted before line <n>]` |
| Metadata line | `start_line`, `returned_lines`, optional `end_line`, `at_eof`, `last_line_partial`, `file_bytes`, `bytes_read`, `encoding` (`utf-8`, `binary`), `lossy`, `line_endings`, `lines_cut`, optional `total_lines` (files of at most 1 MiB), `truncated`, `stop_reason` (`complete`, `max_lines`, `max_bytes`, `result_cap`, `time_budget`, `line_too_long`, `binary`, `start_past_eof`) |
| Semantics | streamed in 64 KiB steps; the first returned line is always returned, cut to `max_bytes` if necessary; a later line that would exceed `max_bytes` ends the slice; after cutting a line the remainder is counted for at most 1 MiB to make `[+N chars]` exact; directory descriptors are dropped before any read |

### text_search

| Element | Contract |
|---|---|
| Input | `query` (required, 1 to 128 characters, no control characters, literal), `path` (max 1,024, default `""`, directory or file), `case_sensitive` (default false, ASCII folding), `max_matches` (1 to 100, default 40), `context_lines` (0 to 2, default 0), `include_hidden` (default false) |
| Text | per file with a reported line: `== <path>`, then `<line>:<text>` for matches and `<line>-<text>` for context, `--` between separated groups |
| Metadata line | `query_chars`, `matches`, `files_scanned`, `files_matched`, `files_skipped.binary`, `.too_large`, `.denied`, `.symlink`, `.hidden`, `.excluded`, `.unreadable`, `.other`, `.too_deep`, `dirs_visited`, `dirs_truncated`, `bytes_scanned`, `truncated`, `stop_reason` (`complete`, `max_matches`, `max_files`, `max_dirs`, `max_entries`, `result_cap`, `scan_budget`, `time_budget`) |
| Bounds | 2,000 files scanned, files over 1 MiB skipped `too_large`, 64 MiB scanned, 2 s scanning, 4,096 directories, 20,000 raw entries, 5,000 entries per directory; each file is read completely into the worker's 1 MiB buffer, so every match line's end is known |

### Workspace tool errors

Tool results with `isError: true` and one fixed text: `missing path`, `invalid path`, `not found`,
`not readable`, `not a file`, `not a directory`, `denied path`, `invalid argument`,
`unexpected argument`, `arguments too large`, `busy` (no job slot within 2 s), `internal error`.
The processing deadline remains `-32603 processing deadline exceeded`; a job that outlives its
handler is orphaned and ends at its next step (see `docs/transport.md`). Calling a workspace tool
without `--root` is `-32602 tool not found`.
