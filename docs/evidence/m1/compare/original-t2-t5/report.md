# Milestone 1 comparison report

Every number is a token count or a byte count; nothing is a cost or a percentage.
Cells report all six measured attempts (failure-inclusive) with denominators; the warm-up attempt is excluded.
A cell is resolved only when attempts 1 to 6 are present exactly once and every one has usage.

## Notes

- ORIGINAL T2 and T5 records under checker versions 1 and 2 (provisional correctness labels); superseded by the reruns in compare/. Preserved unchanged for review.

Checker version: 3

## Claim rule outcomes (arm B versus arm A)

| Client | Task | Verdict | Reasons | total_tokens median delta (B - A) |
|---|---|---|---|---|
| claude | T2 | no saving established | arm B total tokens per correct answer not lower; success-conditioned medians do not point the same way | 12375.0 |
| claude | T5 | no saving established | no correct answers in an arm; success-conditioned medians do not point the same way | -14295.5 |
| codex | T2 | no saving established | arm B total tokens per correct answer not lower; success-conditioned medians do not point the same way | 16724.0 |
| codex | T5 | no saving established | no correct answers in an arm; success-conditioned medians do not point the same way | 16956.5 |

## Cells

| Cell | n | completed | compliant | attributed | correct | partial | usage unavailable | resolution | sum total_tokens | median total_tokens | total_tokens per correct answer | success-conditioned median total_tokens |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| claude/T2/A | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 334799 | 55762.0 | 55799.833333333336 | 55762.0 (success-conditioned, 6 of 6) |
| claude/T2/B | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 434939 | 68137.0 | 72489.83333333333 | 68137.0 (success-conditioned, 6 of 6) |
| claude/T2/C | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 397003 | 56247.0 | 66167.16666666667 | 56247.0 (success-conditioned, 6 of 6) |
| claude/T5/A | 6 | 6 | 6 | 6 | 0 | 6 | 0 | resolved | 516158 | 85448.0 | no correct answers | None (success-conditioned, 0 of 6) |
| claude/T5/B | 6 | 6 | 6 | 6 | 0 | 6 | 0 | resolved | 479220 | 71152.5 | no correct answers | None (success-conditioned, 0 of 6) |
| claude/T5/C | 6 | 6 | 6 | 6 | 0 | 6 | 0 | resolved | 521939 | 86641.0 | no correct answers | None (success-conditioned, 0 of 6) |
| codex/T2/A | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 185389 | 30898.0 | 30898.166666666668 | 30898.0 (success-conditioned, 6 of 6) |
| codex/T2/B | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 284100 | 47622.0 | 47350.0 | 47622.0 (success-conditioned, 6 of 6) |
| codex/T2/C | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 234058 | 39163.5 | 39009.666666666664 | 39163.5 (success-conditioned, 6 of 6) |
| codex/T5/A | 6 | 6 | 6 | 6 | 0 | 6 | 0 | resolved | 184352 | 30890.5 | no correct answers | None (success-conditioned, 0 of 6) |
| codex/T5/B | 6 | 6 | 6 | 6 | 0 | 6 | 0 | resolved | 285431 | 47847.0 | no correct answers | None (success-conditioned, 0 of 6) |
| codex/T5/C | 6 | 6 | 6 | 6 | 0 | 6 | 0 | resolved | 185069 | 31041.0 | no correct answers | None (success-conditioned, 0 of 6) |

## Per-category failure-inclusive medians

| Cell | uncached_input | cache_write | cache_read | output |
|---|---|---|---|---|
| claude/T2/A | 34.0 | 9609.0 | 45955.0 | 182.5 |
| claude/T2/B | 66.0 | 11056.0 | 56528.0 | 485.0 |
| claude/T2/C | 34.0 | 9810.0 | 46197.0 | 206.0 |
| claude/T5/A | 66.0 | 10364.0 | 74075.0 | 943.0 |
| claude/T5/B | 66.0 | 12869.5 | 56486.5 | 1631.0 |
| claude/T5/C | 66.0 | 10878.0 | 74763.0 | 934.0 |
| codex/T2/A | 3330.0 | 0.0 | 27520.0 | 48.0 |
| codex/T2/B | 4150.0 | 0.0 | 43264.0 | 96.0 |
| codex/T2/C | 3705.5 | 0.0 | 36480.0 | 65.0 |
| codex/T5/A | 2264.5 | 0.0 | 28288.0 | 92.5 |
| codex/T5/B | 4948.5 | 0.0 | 42176.0 | 151.5 |
| codex/T5/C | 3420.5 | 0.0 | 27264.0 | 101.0 |

