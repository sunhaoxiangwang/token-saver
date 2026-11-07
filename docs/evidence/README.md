# Evidence pack

Evidence for Milestone 0 is produced by three independent sources that must agree on one call:
the client's own tool-use record, the relay's record of the frames that crossed the server
boundary, and the server's `startup` diagnostic carrying the `instance_id` that the result echoes.

## Relay records

`tools/evidence-relay` records one JSON line per frame with only these fields: `elapsed_ms`
(monotonic since relay start), `unix_ms` (correlation only), `dir`, `bytes`, `kind`, `id_sha256`
(16 hex characters), `method` from a known set or `other`, `tool` from a known set or `other`,
`nonce_match` against the operator's `--expect-nonce`, `nonce_sha256` when it does not match,
`instance_id` when it is 32 lowercase hex characters, `status` when `ok`, `tool_error`,
`error_code`, `protocol_version` and `meta_protocol_version` from a known set or `other`,
`supported_versions`. Raw frames, client names, client versions, reasons, and unknown metadata are
never persisted.

Each run writes `run-N.manifest.json` with `complete: false` before the child starts and sets
`complete: true` only after an orderly end (relay stdin EOF or a forwarded signal), a reaped child,
both forwarding threads ending cleanly (`inbound_outcome` and `outbound_outcome` are `clean_eof`),
flushed and recounted records, zero drops, zero write errors, and storage within budget. Framing
errors, read errors, forwarding failures, a client that stopped reading (`client_output_blocked`),
and early child stdout closure each leave `complete: false` with that reason. The relay waits at
most one second for a forwarding thread after the child is gone, so a blocked client cannot hang it.
Request-id correlation is bounded to 1,024 entries and retires on results, errors, and
cancellations; the manifest reports `pending_correlations`. A capture that is not complete cannot
prove that an invocation did not happen.

Limits: 16 MiB per run, 64 MiB and 64 files per directory including raw artifacts, one controller
per directory enforced with an exclusive lock.

## Client records

Claude Code: the `stream-json` output of `claude -p` (tool_use and tool_result blocks) and a
PostToolUse hook that writes `hook_event_name`, `tool_name`, a SHA-256 prefix of `tool_use_id`,
`duration_ms`, and `nonce_match` only. Codex: the `--json` output of `codex exec` (the
`mcp_tool_call` item). ChatGPT: the tool-call details JSON exported by the operator; when only a user-pasted response is available it is stored as a `user_reported_tool_result` and labelled as weaker evidence than a client export.

## Retention

Relay records, manifests, hook extracts, summaries, and hashes are retained. Raw client artifacts
(debug files, transcripts, terminal recordings) are temporary: operator-only access, 64 MiB each,
summarized, hashed, and deleted when the pack is signed off or after seven days.

## Pack directories

`m0/` holds the pack for the binary hash named in its `artifact-index.json`; packs for superseded
resubmissions are kept as `m0-r<n>/` with their own index so that earlier review findings stay
reproducible. Each index records the raw-artifact hashes, their disposition, and per-file hashes.

## Statuses

Every criterion and test is reported as Passed, Failed, Skipped, Not executed, or Unsupported.
Diagnostic-based assertions are Inconclusive when the diagnostics drop counter is nonzero or absent.
