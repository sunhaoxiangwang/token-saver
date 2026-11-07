# Build and install

## Prerequisites

- Rust toolchain `1.98.1` with `clippy` and `rustfmt`; `rust-toolchain.toml` selects it.
- A Unix host. Milestone 0 was built and verified on macOS 26.6, arm64. Linux is expected to work
  but was not run.
- Network access to crates.io for the first build. `Cargo.lock` pins every dependency; build with
  `--locked` so the resolved graph cannot drift.

## Build

```
cargo build --release --locked
shasum -a 256 target/release/token-saver
```

Record the hash in the evidence pack. Every client proof must reference the same hash.

## Install

The binary is self-contained. Copy it to an absolute path in a directory owned by the operator and
not writable by others, for example `~/.local/bin/token-saver`, and refer to that absolute path in
every client configuration. Do not place it on a shared `PATH` entry that other users can write.

No configuration file, environment variable, or data directory is created or read.

## Verify

```
~/.local/bin/token-saver --version
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}' | ~/.local/bin/token-saver serve --diag off
```

The second command prints one JSON-RPC result naming `supportedVersions` and exits with code 0 when
stdin closes.

## Development tooling

```
cargo test --workspace --no-fail-fast
cargo test --test fixture_lifecycle -- --ignored bookkeeping --nocapture
cargo clippy --workspace --all-targets -- -D warnings
cargo deny --locked check
cargo audit
cargo build --release --locked -p evidence-relay
```

`evidence-relay` is a development binary used only to collect client evidence. It is never installed
beside the server for normal use.

## Remove

Delete the binary and remove its entries from each client, following the removal steps in the
connection documents. The server leaves nothing else behind.
