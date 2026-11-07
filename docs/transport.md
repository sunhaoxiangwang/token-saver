# Transport, registry, and supervision

This document describes how `src/transport.rs` and `src/supervisor.rs` bound every request from the
first byte read to the last byte written, and how the process is forced to fail when a bound is
exceeded. The SDK (`rmcp 3.9.0`) owns protocol semantics, lifecycle negotiation, and dispatch; the
transport adapter owns bytes, framing, classification, admission, delivery, and termination.

## Threads

| Thread | Role | Blocks on |
|---|---|---|
| reader | frames stdin at 64 KiB, validates envelopes, reserves ids, applies cancellations, builds protocol-error frames | stdin only |
| runtime (tokio, current thread) | the SDK service loop; `receive` admits requests, `send` commits responses | nothing outside the runtime |
| writer | writes and flushes one frame at a time, then retires its record | stdout only |
| supervisor | evaluates deadlines from atomics every 100 ms; forces exit | nothing |
| diagnostics | writes allowlisted events to stderr | stderr only |

## Request lifecycle

```
 bytes ──reader──► FRAMED ──invalid──► REJECTED (error frame, error-budget permit) ──ack──► retired
                     │ valid request: id reserved under the registry lock BEFORE publication
                     ▼
                  QUEUED ──cancel──► QUEUED_CANCELLED ──dispatch──► removed, no response
                     │ adapter takes it from the handoff channel, permit acquired
                     ▼
                  ADMITTED ──cancel──► CANCELLED ──SDK output──► removed, output suppressed
                     │ SDK calls send(): commitment point, delivery record created
                     ▼
                  COMMITTED ──cancel──► COMMITTED (late, recorded) ──writer ack──► RETIRED
```

Transitions happen inside one registry critical section each, so cancellation and commitment cannot
interleave. A duplicate of any unretired id, whatever the frame's validity, terminates the
connection. Permits are released only at retirement: writer acknowledgment, or suppression of a
cancelled request's output, which the SDK produces only after the handler returned.

## Ownership and buffers

| Buffer or resource | Owner | Bound |
|---|---|---|
| kernel pipes | OS | OS-controlled |
| line buffer | reader | 65,536 bytes |
| request slots | registry | 9 requests plus up to 4 rejected-frame slots |
| handoff channel | reader to adapter | 9, never the limiting resource |
| staged request | adapter `self` | 1, survives a dropped `receive` future |
| admission permits | adapter, then slot | 8, acquired only when a request exists |
| error budget | reader, then delivery record | 4 |
| delivery records | registry | 12, each at most 16,384 bytes |
| outbound channel | adapter and reader to writer | 12 |
| `initialized` forwarding | reader | one outstanding, credited per committed initialize response, acknowledged by `on_initialized` |
| diagnostics queue | sink | 256 events |

The 16,384-byte frame bound is checked on both publication paths before a frame enters the
outbound channel: in `send` for SDK responses and errors, and in the reader for the errors the
adapter generates itself (parse, envelope, version, and params rejections). An oversized frame is
never published; the reader-generated path has the same exposure as the SDK path because the
version error echoes the caller's `requested` string. Either path emits `outbound.oversized`,
latches `outbound_too_large`, and the process exits 3.

Not enforced by this crate: the SDK's completed send-task records, its 64-slot proxy channel, and
its handler-task bookkeeping. These are measured (see benchmark methodology), not asserted.

## Budgets

| Budget | Starts at | Enforced by |
|---|---|---|
| processing, 5 s | `admit_at` | handler returns `-32603` |
| delivery, 5 s | `commit_at` | supervisor, exit 4 |
| unresolved admission, 10 s | `admit_at`, kept through delivery | supervisor, exit 4 |
| acceptance to admission, 10 s | `accepted_at` | supervisor, exit 4 |
| registry transaction, 1 s | before `lock()` | supervisor, exit 4 |
| startup, 3 s | process start | supervisor, exit 4 |
| shutdown, 3 s | first trigger | supervisor, exit 1 or the latched fatal code |

The acceptance budget equals the unresolved-admission budget, so a queued request is admitted no
later than the forced failure of the request whose permit it waits for; it fires first only when
the runtime stops dispatching. Worst case for one accepted request: 10 s queued, 10 s admitted,
3 s shutdown, plus one 100 ms interval.

## Deadline publication

The supervisor never takes the registry lock. The registry publishes, as the last statement of every
critical section, one 64-bit word: 56 bits of monotonic nanoseconds since start and an 8-bit kind
tag for the earliest outstanding acceptance, admission, or delivery deadline. One store makes every
snapshot coherent. Each participant stores a transaction marker (`now + 1 s`) before acquiring the
lock and clears it after release, so a stall anywhere inside a transaction, including before
publication, is detected. Encoding is checked: the supported uptime is 800 days, below the 834-day
range of the word, and reaching it latches `uptime_limit`.

## Exit cause rule

`fatal_cause` is a compare-and-swap latch over fatal causes only; the first fatal cause wins.
`orderly_trigger` records the first of EOF, SIGINT, SIGTERM separately. Exit code: the fatal cause's
code if one is latched, else 0 for orderly completion, else 1 when the supervisor forces an orderly
shutdown at the 3 s budget. The supervisor terminates with `std::process::exit` without joining
threads or taking locks, after enqueueing one diagnostic without blocking.

## EOF

EOF stops reading, not serving. Requests accepted before EOF stay in the handoff channel and are
dispatched as permits allow; `receive` returns `None` only when the reader has exited and the
channel is empty. `close` then waits until every delivery record is acknowledged by the writer.
`main` selects exit 0 only when no request and no delivery remains; otherwise it waits until the
shutdown budget, at which point the supervisor forces exit 1. Tested against the binary with a
request followed by immediate EOF, a burst of nine requests followed by EOF, and EOF with a saturated
stdout pipe.

## Notifications

`notifications/cancelled` is applied by the reader directly and never forwarded, so the SDK produces
the handler's response and the adapter suppresses it, proving handler completion.
`notifications/initialized` is forwarded only while a credit exists and none is outstanding. Every
other notification and every client response is dropped with a counted diagnostic.
