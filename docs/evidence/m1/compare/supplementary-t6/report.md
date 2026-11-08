# Milestone 1 comparison report

Every number is a token count or a byte count; nothing is a cost or a percentage.
Cells report all six measured attempts (failure-inclusive) with denominators; the warm-up attempt is excluded.
A cell is resolved only when attempts 1 to 6 are present exactly once and every one has usage.

## Notes

- SUPPLEMENTARY complete rerun of Claude Code task T6 (arms A, B, C, attempts 0 to 6) on 2026-11-07 with the host kept awake (caffeinate), after host sleep (clamshell, 15:32 to about 18:10 local) interrupted four attempts of the primary T6/B cell, two of which failed with exit 1 and zero usage. The primary table is unchanged; this run adds information and is not used for any claim in compare/report.md.

Checker version: 3

## Claim rule outcomes (arm B versus arm A)

| Client | Task | Verdict | Reasons | total_tokens median delta (B - A) |
|---|---|---|---|---|
| claude | T6 | no saving established | arm B total tokens per correct answer not lower; success-conditioned medians do not point the same way | 8823.0 |

## Cells

| Cell | n | completed | compliant | attributed | correct | partial | usage unavailable | resolution | sum total_tokens | median total_tokens | total_tokens per correct answer | success-conditioned median total_tokens |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| claude/T6/A | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 539695 | 84956.0 | 89949.16666666667 | 84956.0 (success-conditioned, 6 of 6) |
| claude/T6/B | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 562093 | 93779.0 | 93682.16666666667 | 93779.0 (success-conditioned, 6 of 6) |
| claude/T6/C | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 645284 | 118263.0 | 107547.33333333333 | 118263.0 (success-conditioned, 6 of 6) |

## Per-category failure-inclusive medians

| Cell | uncached_input | cache_write | cache_read | output |
|---|---|---|---|---|
| claude/T6/A | 66.0 | 10347.5 | 73891.0 | 667.0 |
| claude/T6/B | 98.0 | 12365.0 | 80474.0 | 848.0 |
| claude/T6/C | 98.0 | 12053.0 | 102609.0 | 925.5 |

