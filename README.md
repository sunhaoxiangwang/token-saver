# token-saver

A minimal Model Context Protocol (MCP) server over stdio. Without `--root` it exposes one
read-only tool, `connection_probe` (Milestone 0: reachable, discoverable, and callable from ChatGPT
Web through OpenAI's Secure MCP Tunnel, from Claude Code over direct stdio, and from Codex CLI over
direct stdio, with evidence that the assistant used this server). With `--root /abs/project` it also
exposes the Workspace Reading Pack (Milestone 1): `workspace_outline`, `file_slice`, and
`text_search`, three read-only tools that return compact, bounded views of a project tree from an
owned worker pool over the root directory descriptor.

No token saving is claimed by this repository except as the measured outcome of the registered
comparison in `docs/evidence/m1/`, under its pre-registered rule.

## Status

The local implementation was approved on 2026-11-06 (TS-M0-CODE-REVIEW-003) with automated evidence
from Claude Code and Codex CLI in `docs/evidence/m0/`. The ChatGPT probe through tunnel-client 0.0.20 was performed by the operator on 2026-11-06 and is
corroborated by the relay capture and the user-reported result (`docs/evidence/m0/chatgpt.*`). Full
milestone acceptance is pending the remaining checks: ChatGPT's own tool-call export, the helper's
startup stderr, the Platform audit entries, account eligibility evidence, and the interactive
client procedures. For ChatGPT, the tunnel helper daemon must remain running for every
call; see `docs/connect-chatgpt-tunnel.md`.

## What it does

- Speaks MCP `2026-07-28` (per-request metadata, `server/discover`) and `2025-11-25` (the
  `initialize` handshake). Older handshake versions are answered with `2025-11-25`.
- Serves `connection_probe`: pass a `nonce` of 1 to 64 ASCII letters or digits; the result echoes it
  with `instance_id`, `server_version`, and `status: "ok"`.
- With `--root`, serves `workspace_outline`, `file_slice`, and `text_search` over one directory:
  descriptor-relative, no-follow resolution (`docs/adr/0002-retained-directory-authority.md`), four
  owned workers with eight job slots supervised step by step, results capped at 12,288 bytes by
  bounded construction, text-only with a final `#` metadata line.
- Enforces fixed bounds on frames, ids, arguments, results, admissions, deliveries, deadlines, and
  diagnostics, supervised by a watchdog thread that runs outside the async runtime.
- Reads no files without `--root`; with it, reads regular files and directories below the root
  only. Reads no environment variables or configuration; opens no sockets; spawns nothing.

## Build and test

```
cargo build --release --locked
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets -- -D warnings
cargo deny --locked check && cargo audit
```

Toolchain is pinned in `rust-toolchain.toml`. Verified on macOS 26.6 (arm64) with Rust 1.98.1.

## Run

```
target/release/token-saver serve [--diag off|error|info|debug] [--root /abs/project]
target/release/token-saver --version
```

The server reads JSON-RPC frames on stdin and writes responses on stdout. Diagnostics go to stderr.

## Documentation

| Document | Content |
|---|---|
| [docs/install.md](docs/install.md) | Build, install location, hash recording, removal |
| [docs/configuration.md](docs/configuration.md) | Flags, fixed limits, exit codes, client configuration |
| [docs/tools.md](docs/tools.md) | The probe contract and the error contract |
| [docs/transport.md](docs/transport.md) | Request lifecycle, ownership, budgets, supervision |
| [docs/security.md](docs/security.md) | Trust boundaries, threat model, launch environment |
| [docs/connect-claude-code.md](docs/connect-claude-code.md) | Claude Code procedures and evidence |
| [docs/connect-chatgpt-tunnel.md](docs/connect-chatgpt-tunnel.md) | ChatGPT Web through the Secure MCP Tunnel |
| [docs/connect-codex.md](docs/connect-codex.md) | Codex CLI procedures and evidence |
| [docs/compatibility.md](docs/compatibility.md) | Tested support versus SDK capability |
| [docs/troubleshooting.md](docs/troubleshooting.md) | Symptoms, causes, checks |
| [docs/limitations.md](docs/limitations.md) | Known limits of Milestone 0 |
| [docs/benchmark-methodology.md](docs/benchmark-methodology.md) | How measurements are taken |
| [docs/evidence/README.md](docs/evidence/README.md) | Evidence pack format and retention |
| [docs/adr/0001-rmcp-dual-era-single-binary.md](docs/adr/0001-rmcp-dual-era-single-binary.md) | Architecture decision record: one dual-era binary |
| [docs/adr/0002-retained-directory-authority.md](docs/adr/0002-retained-directory-authority.md) | Architecture decision record: workspace root authority |
| [tools/token-compare/README.md](tools/token-compare/README.md) | The Milestone 1 comparison harness |

## Layout

- `src/` production crate: `main.rs`, `probe.rs`, `transport.rs`, `diag.rs`, `supervisor.rs`,
  `workspace/` (`confine.rs`, `worker.rs`, `policy.rs`, `render.rs`, `outline.rs`, `slice.rs`,
  `search.rs`)
- `tests/` SDK-independent wire tests, in-process fixture tests (including the gated workspace
  filesystem), schema tests
- `tools/evidence-relay/` development-only relay that records allowlisted evidence fields
- `tools/token-compare/` the comparison harness (Python, standard library)

## License

Apache-2.0. See [LICENSE](LICENSE) and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
