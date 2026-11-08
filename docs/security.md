# Security

## What the server holds

Without `--root`: nothing privileged. It reads no files, no environment variables, and no
configuration; opens no sockets; spawns no processes; keeps no state beyond process lifetime. Its
only exposures are CPU and memory on the host and the four probe fields, which exclude hostname,
user, paths, process id, and environment.

With `--root`: one directory descriptor, opened once at startup, and the retained-directory
authority it confers (`docs/adr/0002-retained-directory-authority.md`): the workspace tools read
regular files and list directories that are, at the instant of each open, direct children of a
directory object a job holds, starting from the root. Symlinks are never followed, special files are
never read, writes and execution do not exist, and nothing is retained between jobs. The textual
root never appears in results or diagnostics. The content below the root reaches the model and, for
ChatGPT, OpenAI: declare project roots only, never a home directory, and ChatGPT roots only with
content you would upload. The exclusion and denial lists of `docs/tools.md` are documented guards
(TS-M1-REVIEW-002, D2), not a guarantee that no secret inside a root is readable.

## Trust boundaries

| Path | Who can reach the process | Authority granted |
|---|---|---|
| Claude Code, stdio | the local Claude Code process that spawned it | none beyond the probe; with `--root`, read access below the declared root under the authority contract |
| Codex CLI, stdio | the local Codex process | the same |
| ChatGPT Web via tunnel-client | any ChatGPT user in the owning workspace who can install the plugin, through OpenAI infrastructure and the local helper | the same; content read below a ChatGPT root leaves the machine |

Client-provided identity (`clientInfo`), `_meta`, and MCP roots are never used for decisions and are
not echoed. The only client values that appear in a result are the validated nonce (probe) and the
content and names read below the declared root (workspace tools); arguments are never echoed.

## Launch environment, from pinned sources and documentation

| Aspect | Claude Code | Codex CLI 0.167.0 | tunnel-client (0.0.19 source; 0.0.20 installed, not yet run) |
|---|---|---|---|
| Argument encoding | `command` plus `args`, no shell | `command` plus `args`, no shell | whitespace split with shell-style quotes, then `exec.Command`, no shell |
| Working directory | project directory for local scope | configured `cwd`, else session directory | inherited from the helper |
| Environment | inherits Claude's environment plus `env` and `CLAUDE_PROJECT_DIR` | only `HOME`, `LOGNAME`, `PATH`, `SHELL`, `USER`, `__CF_USER_TEXT_ENCODING`, `LANG`, `LC_ALL`, `TERM`, `TMPDIR`, `TZ`, plus configured `env` and `env_vars` | inherited from the helper |
| Child stderr | captured by the client | logged line by line at `info` as "MCP server stderr" | forwarded to the helper's stderr |
| Shutdown | SIGINT, SIGTERM after 100 ms, SIGKILL after 400 ms more | close, wait 3 s, kill; process group SIGTERM with 2 s grace | close stdin and stdout, SIGTERM; kill after 1 s on retire |

token-saver's configuration reads no environment variables and its output contains no environment
data. Dependencies may read the environment, and the child's environment may contain inherited
secrets under Claude Code and tunnel-client. Launch the tunnel daemon from a shell with a minimal
environment and keep the runtime key in a `file:` reference with mode 0600 rather than an exported
variable; that keeps one secret out of the environment and nothing more.

## Credentials

The server holds none. The only credentials in the ChatGPT path are OpenAI's runtime key and the
tunnel id, both in the operator's tunnel-client profile. Revocation: revoke the runtime key on the
Platform, delete the tunnel, uninstall the plugin, stop the daemon. Claude Code and Codex hold no
credentials for this server. No credential appears in the repository, in arguments, or in
diagnostics; tests inject canaries into every client-controlled position and assert their absence
from stderr at every diagnostics level.

## Configuration integrity

Install the binary at an absolute path in an operator-owned directory and record its SHA-256.
Register it at Claude Code local scope (`~/.claude.json`) rather than a project `.mcp.json`, so no
project trust prompt and no committed configuration are involved. Codex configuration lives in
`~/.codex/config.toml`. The tunnel-client profile and key file are mode 0600.

## Diagnostics and evidence policy

Diagnostics carry fixed fields only, never ids, method strings outside the known set, reason text,
paths, names, or payload; the SDK's log events are disabled wholesale. The evidence relay persists
allowlisted fields only, hashes any value it must correlate (request ids, tool arguments as a
canonical-JSON digest, result text as a digest with its byte size), persists a nonce only when it
equals the operator's expected nonce for that run, never persists client name or version, and marks a
run complete only after an orderly end with every record flushed and recounted. Raw client
artifacts are temporary traces under the operator's control: hashed, reduced to sanitized records,
scanned for canaries, the fixture path, the home directory, and account identifiers, then deleted.

## Threat model

| # | Threat | Vector | Mitigation | Residual |
|---|---|---|---|---|
| T1 | malformed or oversized input or output | stdin, stdout | 64 KiB inbound frame bound closes the connection; independent envelope validation; argument bound; 16,384-byte outbound bound on every frame including adapter-generated errors | none beyond a closed connection |
| T2 | handler task flood | many requests | admission before SDK spawn, 8 admitted, 1 pending, overflow closes | none |
| T3 | information disclosure | probe result | four fields, no host identifiers | version and instance id are visible |
| T4 | unauthorized caller via workspace | tunnel | nothing privileged exists | CPU and queue use |
| T5 | runtime key leakage | environment, arguments, logs | `file:` reference, no environment reads, no payload diagnostics | key readable by the local user by design |
| T6 | binary substitution | writable path | absolute path, hash recorded | local user compromise out of scope |
| T7 | configuration tampering | client config files | operator-owned, 0600, uncommitted | same as T6 |
| T8 | denial of service | floods | bounds, deadlines, error budget, supervisor termination; upstream tunnel rate limits | host CPU during a flood |
| T9 | handshake desync under tunnel multiplexing | repeated initialize | SDK re-negotiates; tested | none |
| T10 | stdout corruption | stray print | compile-time deny of print macros; stdout purity test | none |
| T11 | SDK payload events reaching stderr | tracing | no subscriber installed; canary tests | none |
| T12 | supply chain | transitive crates | exact pin, lockfile, `cargo deny`, `cargo audit`, notices | upstream compromise |
| T13 | lingering process | ignored signals, stuck work | EOF and signal handling, external supervisor, forced exit | client kill fallback |
| T14 | data transit through OpenAI | tunnel | probe data non-sensitive | inherent |
| T15 | deadline bypass by uncooperative work | handler ignores tokens | supervisor forces exit at 10 s | none |
| T16 | stalled registry or runtime | lock held, blocked thread | transaction markers, published deadlines, exit without locks | none |
| T17 | id confusion after timeout | reuse | ids owned until retirement, duplicates terminate | none |
| T18 | path escape from the root | `..`, absolute paths, symlinks, component replacement | lexical validation; descriptor-relative single-component `openat` with `O_NOFOLLOW`; classification before open; `fstat` after open | retained-directory authority: a directory held by a running job and moved out of the root stays readable to that job until the job ends (C8); move-in and hard links inside the root are inside |
| T19 | secret exposure inside a root | `.env`, keys, credentials | denial list, exclusion list, operator guidance (project roots only) | the lists are guards, not a guarantee |
| T20 | blocking on special files | FIFOs, sockets, devices | `fstatat` classification before any open; every open `O_NONBLOCK|O_NOCTTY`; a raced special file is identified by `fstat` and closed unread (C9 to C11) | none observed; device nodes need privilege to create |
| T21 | blocking work outliving its handler | slow or stalled reads | owned workers, job slots released at completion, orphans counted, step marker 2 s and job marker deadline + 2.5 s latch `worker_stall` (S1 to S7) | a step admitted before the latch may finish after it |
| T22 | stalled filesystem | network volume, hung device | step budget 2 s from the step's start, exit 4 | the process ends, as for every stall in Milestone 0 |
| T23 | oversized or content-leaking results | long lines, control characters, deep paths | bounded construction under a 12,288-byte result cap with exact escaped-length accounting; metadata never carries content, names, or arguments; generated paths bounded like arguments | none |
