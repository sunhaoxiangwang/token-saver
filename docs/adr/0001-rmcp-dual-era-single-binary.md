# ADR-0001: single dual-era Rust binary on rmcp 3.9.0 with per-client stdio instances

Status: accepted at plan level (TS-M0-REVIEW-006), implemented in Milestone 0.

## Context

Two required clients and one supplementary client reach the server over stdio at the server boundary:
ChatGPT Web through OpenAI's tunnel-client (documented as using the `initialize` handshake), Claude
Code (asks stdio servers for `2026-07-28` on its v2 runtime), and Codex CLI (`initialize` at
`2025-06-18` by default). The server must be stable Rust, one executable, minimal dependencies,
bounded in allocation, queues, output, and logging, with protocol-only stdout.

## Decision

- One executable, `token-saver`, run as a separate process per client. No shared process or state.
- `rmcp = "=3.9.0"` with the `server` feature only, for protocol types, lifecycle negotiation,
  `server/discover`, metadata validation, and dispatch. Advertised versions restricted to
  `2025-11-25` and `2026-07-28`.
- A custom `Transport` implementation, `BoundedStdio`, as the single extension point: framing,
  independent envelope validation, id reservation before publication, admission, commitment,
  delivery records, and termination. No fork of the SDK.
- A hand-written `ServerHandler` instead of the router macros, so argument errors are fixed, bounded
  texts and schemas are hand-written constants.
- Diagnostics through an allowlisted sink; no tracing subscriber installed, so SDK log events never
  reach stderr.
- A supervisor thread outside the async runtime, reading published atomics only, with latched fatal
  causes separate from orderly triggers.
- Evidence collected by a development-only relay, never by the production binary.

## Consequences

- SDK release churn is absorbed by an exact pin, a committed lockfile, and SDK-independent wire tests.
- Capacity is explicit and small: 8 admitted plus 1 pending request; a tenth closes the connection.
- The adapter adds about one thousand lines that must be maintained alongside the SDK's transport
  contract.
- SDK internal bookkeeping is measured, not enforced.

## Alternatives rejected

- Hand-rolled protocol: full control, but reimplements negotiation rules the SDK already encodes.
- Modern-only server: fails the documented ChatGPT and Codex handshakes.
- Shared process for all clients: violates isolation and makes per-client evidence ambiguous.
- Production capture flag: adds a filesystem capability to a server that must have none.
- Router macros: unbounded serde error text in tool results.
