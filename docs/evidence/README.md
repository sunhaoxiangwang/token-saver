# Evidence pack

Evidence for Milestone 0 is produced by three independent sources that must agree on one call:
the client's own tool-use record, the relay's record of the frames that crossed the server
boundary, and the server's `startup` diagnostic carrying the `instance_id` that the result echoes.

## Relay records

`tools/evidence-relay` records one JSON line per frame with only these fields: `elapsed_ms`
(monotonic since relay start), `unix_ms` (correlation only), `dir`, `bytes`, `kind`, `id_sha256`
(16 hex characters), `method` from a known set or `other`, `tool` from the known set
(`connection_probe`, `workspace_outline`, `file_slice`, `text_search`) or `other`, `args_sha256`
and `args_bytes` (16 hex characters of SHA-256 over the canonical JSON of `arguments` and its byte
length; `args_sha256: null` with `args_canonical: false` when a number is not an integer),
`nonce_match` against the operator's `--expect-nonce`, `nonce_sha256` when it does not match,
`instance_id` when it is 32 lowercase hex characters, `status` when `ok`, `is_error`, `tool_error`,
`result_sha256` and `result_bytes` (over the UTF-8 bytes of `result.content[0].text`; `null` for a
JSON-RPC error), `error_code`, `protocol_version` and `meta_protocol_version` from a known set or
`other`, `supported_versions`. Raw frames, arguments, paths, names, content, client names, client
versions, reasons, and unknown metadata are never persisted.

Canonical JSON: keys sorted by code point, no whitespace, integers in decimal, `true`, `false`,
`null`, strings escaping only `"`, `\\`, and control characters (`\\b`, `\\t`, `\\n`, `\\f`,
`\\r`, else `\\u00xx` lowercase), everything else as raw UTF-8. The same rule is implemented in the
relay (Rust), the hook extractor, and the comparison harness (Python); the eight vectors of
TS-M1-PLAN-001 section 7.3 are asserted in all three.

Each run writes `run-N.manifest.json` with `complete: false` before the child starts and sets
`complete: true` only after an orderly end (relay stdin EOF or a forwarded signal), a reaped child,
the outbound thread ending cleanly (`outbound_outcome: clean_eof`), the inbound thread ending
cleanly or being quiescent at the capture boundary (`inbound_outcome: clean_eof` or `quiescent`),
no frame left in flight at the boundary (`in_flight_unresolved: 0`), flushed and recounted records,
zero drops, zero write errors, and storage within budget. Framing
errors, read errors, forwarding failures, a client that stopped reading (`client_output_blocked`),
early child stdout closure, a frame admitted before the boundary but not published within the drain
(`frame_in_flight`), and input still pending at the capture boundary (`input_pending`) each leave
`complete: false` with that reason. The capture boundary is synchronized through the recorder lock:
every frame is admitted and later published under that lock; when the run ends (EOF, signal, child
exit) the recorder is sealed (no new admissions: later frames are counted in `late_frames`, never
recorded or forwarded), frames admitted before the seal are given 150 ms to publish, then the
recorder is closed and nothing can be written (a frame that still reaches publication is counted in
`dropped_in_flight`). The manifest reports `in_flight_at_seal` and `in_flight_unresolved`. An input
that is still open is accepted only when stdin has no data pending for 150 ms at the boundary
(`inbound_outcome: quiescent`; the poll is retried when a second client signal interrupts it); a
hang-up or readable stdin gives the inbound thread 150 ms to report EOF (`clean_eof`); otherwise the
run is incomplete with `input_pending`. The relay waits at
most one second for a forwarding thread after the child is gone, so a blocked client cannot hang it.
Request-id correlation is bounded to 1,024 entries and retires on results, errors, and
cancellations; the manifest reports `pending_correlations`. A capture that is not complete cannot
prove that an invocation did not happen.

Limits: 16 MiB per run, 64 MiB and 64 files per directory including raw artifacts, one controller
per directory enforced with an exclusive lock.

## Client records

Claude Code: the `stream-json` output of `claude -p` (tool_use and tool_result blocks) and a
PostToolUse hook that writes `hook_event_name`, `tool_name`, a SHA-256 prefix of `tool_use_id`,
`duration_ms`, `nonce_match`, and (Milestone 1) `tool_input_sha256`, `tool_input_bytes`,
`tool_response_sha256`, `tool_response_text_bytes`. The hook fires for successful calls only;
results with `isError: true` produce no PostToolUse event (observed 2026-11-07), so the hook extract
is a subset of the stream, which with the relay covers every call. Codex: the `--json` output of `codex exec` (the
`mcp_tool_call` item). ChatGPT: the tool-call details JSON exported by the operator; when only a user-pasted response is available it is stored as a `user_reported_tool_result` and labelled as weaker evidence than a client export.

## Retention

Two classes. Temporary raw traces (client transcripts and streams, stderr, hook payloads, fixture
copies, raw answer text) are operator-only, hashed, reduced to sanitized records, and deleted; they
never enter `docs/`. Retained sanitized artifacts (relay records and manifests, hook extracts,
attempt records with digests, sizes, usage, verdicts and raw hashes, summaries, indexes) must be free
of the canaries `CANARY_DIR_7f3a` and `CANARY_CONTENT_9c1e`, the fixture's absolute path, the home
directory, and account identifiers; the harness scans every retained file and the relay manifest's
argv paths are replaced by placeholders before retention. Legacy exception: the six Milestone 0 relay
manifests (`m0/chatgpt.relay.manifest.json`, `m0/claude-auto.run-1.relay.manifest.json`,
`m0/claude-auto.run-2.relay.manifest.json`, `m0/codex-auto.relay.manifest.json`,
`m0-r2/claude-auto.relay.manifest.json`, `m0-r2/codex-auto.relay.manifest.json`) were retained before
this rule existed and carry the host's absolute paths in their `server` argv. They are preserved
unchanged so that the Milestone 0 pack hashes in their indexes stay reproducible; no later pack
repeats the exception. A canary in a raw trace after a tool
legitimately returned it is the positive control that the extractor removes content.

## Pack directories

`m0/` holds the Milestone 0 pack for the binary hash named in its `artifact-index.json`; packs for
superseded resubmissions are kept as `m0-r<n>/` with their own index so that earlier review findings
stay reproducible. `m1/` holds the Milestone 1 proofs (`proofs/`) and the comparison (`compare/`:
`report.json`, `report.md`, one attempt record per run) for the historical binary hash named in its
index, the corrected build's verification proofs and checks (`verification/`), and the ChatGPT
operator capture (`chatgpt/`), each block of the index naming the binary and relay hashes it was
produced with.
Each index records the raw-artifact hashes, their disposition, and per-file hashes.

## Statuses

Every criterion and test is reported as Passed, Failed, Skipped, Not executed, or Unsupported.
Diagnostic-based assertions are Inconclusive when the diagnostics drop counter is nonzero or absent.
