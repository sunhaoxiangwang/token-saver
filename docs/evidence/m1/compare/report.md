# Milestone 1 comparison report

Every number is a token count or a byte count; nothing is a cost or a percentage.
Cells report all six measured attempts (failure-inclusive) with denominators; the warm-up attempt is excluded.
A cell is resolved only when attempts 1 to 6 are present exactly once and every one has usage.

## Notes

- T2 cells were rerun on 2026-11-07 with checker version 2 after harness finding F02, and T5 cells with checker version 3 after harness finding F03 (both clients, arms A, B, C, attempts 0 to 6); the original T2 and T5 records (provisional labels under earlier checker versions) are preserved in compare/original-t2-t5/ and were not regraded because raw answers are deleted by design. T1, T3, T4, T6 were not rerun. Environment: the host entered clamshell sleep at 15:32 local and dark-woke repeatedly until about 18:10 while the primary Claude Code T6/B cell ran; attempts a0, a1, a2, a6 of that cell span sleep (wall times over 180 s although the monotonic 180 s timeout never fired); a1 and a6 exited 1 with zero usage and are treated as usage unavailable, so the cell is unresolved. A supplementary complete T6 rerun is in compare/supplementary-t6/ and is not used for claims.

Checker version: 3

## Claim rule outcomes (arm B versus arm A)

| Client | Task | Verdict | Reasons | total_tokens median delta (B - A) |
|---|---|---|---|---|
| claude | T1 | no saving established | arm B total tokens per correct answer not lower; success-conditioned medians do not point the same way | 21499.5 |
| claude | T2 | no saving established | arm B total tokens per correct answer not lower; success-conditioned medians do not point the same way | 37250.0 |
| claude | T3 | no saving established | arm B total tokens per correct answer not lower; success-conditioned medians do not point the same way | 9898.5 |
| claude | T4 | no saving established | arm B total tokens per correct answer not lower; success-conditioned medians do not point the same way | 11904.0 |
| claude | T5 | no saving established | arm B total tokens per correct answer not lower; success-conditioned medians do not point the same way | 14580.5 |
| claude | T6 | unresolved | arm B has 2 attempt(s) with usage unavailable | 8154.0 |
| codex | T1 | no saving established | arm B total tokens per correct answer not lower; success-conditioned medians do not point the same way | 16243.5 |
| codex | T2 | no saving established | arm B total tokens per correct answer not lower; success-conditioned medians do not point the same way | 17289.0 |
| codex | T3 | no saving established | arm B total tokens per correct answer not lower; success-conditioned medians do not point the same way | 16657.0 |
| codex | T4 | no saving established | arm B total tokens per correct answer not lower; success-conditioned medians do not point the same way | 36365.5 |
| codex | T5 | no saving established | arm B total tokens per correct answer not lower; success-conditioned medians do not point the same way | 16915.5 |
| codex | T6 | no saving established | arm B total tokens per correct answer not lower; success-conditioned medians do not point the same way | 17956.0 |

## Cells

| Cell | n | completed | compliant | attributed | correct | partial | usage unavailable | resolution | sum total_tokens | median total_tokens | total_tokens per correct answer | success-conditioned median total_tokens |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| claude/T1/A | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 467106 | 86123.0 | 77851.0 | 86123.0 (success-conditioned, 6 of 6) |
| claude/T1/B | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 634573 | 107622.5 | 105762.16666666667 | 107622.5 (success-conditioned, 6 of 6) |
| claude/T1/C | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 585171 | 87327.0 | 97528.5 | 87327.0 (success-conditioned, 6 of 6) |
| claude/T2/A | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 335296 | 55877.0 | 55882.666666666664 | 55877.0 (success-conditioned, 6 of 6) |
| claude/T2/B | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 510431 | 93127.0 | 85071.83333333333 | 93127.0 (success-conditioned, 6 of 6) |
| claude/T2/C | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 428800 | 71494.5 | 71466.66666666667 | 71494.5 (success-conditioned, 6 of 6) |
| claude/T3/A | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 345826 | 57631.0 | 57637.666666666664 | 57631.0 (success-conditioned, 6 of 6) |
| claude/T3/B | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 408781 | 67529.5 | 68130.16666666667 | 67529.5 (success-conditioned, 6 of 6) |
| claude/T3/C | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 501334 | 88471.0 | 83555.66666666667 | 88471.0 (success-conditioned, 6 of 6) |
| claude/T4/A | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 369360 | 56806.5 | 61560.0 | 56806.5 (success-conditioned, 6 of 6) |
| claude/T4/B | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 412861 | 68710.5 | 68810.16666666667 | 68710.5 (success-conditioned, 6 of 6) |
| claude/T4/C | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 372792 | 57156.5 | 62132.0 | 57156.5 (success-conditioned, 6 of 6) |
| claude/T5/A | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 397156 | 56230.0 | 66192.66666666667 | 56230.0 (success-conditioned, 6 of 6) |
| claude/T5/B | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 451985 | 70810.5 | 75330.83333333333 | 70810.5 (success-conditioned, 6 of 6) |
| claude/T5/C | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 462815 | 86691.0 | 77135.83333333333 | 86691.0 (success-conditioned, 6 of 6) |
| claude/T6/A | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 509369 | 84799.0 | 84894.83333333333 | 84799.0 (success-conditioned, 6 of 6) |
| claude/T6/B | 6 | 4 | 6 | 6 | 4 | 0 | 2 | unresolved | 371457 | 92953.0 | 92864.25 | 92953.0 (success-conditioned, 4 of 6) |
| claude/T6/C | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 708949 | 118072.0 | 118158.16666666667 | 118072.0 (success-conditioned, 6 of 6) |
| codex/T1/A | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 189073 | 31507.0 | 31512.166666666668 | 31507.0 (success-conditioned, 6 of 6) |
| codex/T1/B | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 284853 | 47750.5 | 47475.5 | 47750.5 (success-conditioned, 6 of 6) |
| codex/T1/C | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 189747 | 31624.5 | 31624.5 | 31624.5 (success-conditioned, 6 of 6) |
| codex/T2/A | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 182137 | 30356.0 | 30356.166666666668 | 30356.0 (success-conditioned, 6 of 6) |
| codex/T2/B | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 287652 | 47645.0 | 47942.0 | 47645.0 (success-conditioned, 6 of 6) |
| codex/T2/C | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 182741 | 30457.5 | 30456.833333333332 | 30457.5 (success-conditioned, 6 of 6) |
| codex/T3/A | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 190482 | 31747.0 | 31747.0 | 31747.0 (success-conditioned, 6 of 6) |
| codex/T3/B | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 290424 | 48404.0 | 48404.0 | 48404.0 (success-conditioned, 6 of 6) |
| codex/T3/C | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 191154 | 31859.0 | 31859.0 | 31859.0 (success-conditioned, 6 of 6) |
| codex/T4/A | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 204776 | 34739.5 | 34129.333333333336 | 34739.5 (success-conditioned, 6 of 6) |
| codex/T4/B | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 424242 | 71105.0 | 70707.0 | 71105.0 (success-conditioned, 6 of 6) |
| codex/T4/C | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 193438 | 31758.5 | 32239.666666666668 | 31758.5 (success-conditioned, 6 of 6) |
| codex/T5/A | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 185481 | 30907.0 | 30913.5 | 30907.0 (success-conditioned, 6 of 6) |
| codex/T5/B | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 286123 | 47822.5 | 47687.166666666664 | 47822.5 (success-conditioned, 6 of 6) |
| codex/T5/C | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 182970 | 30502.5 | 30495.0 | 30502.5 (success-conditioned, 6 of 6) |
| codex/T6/A | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 186576 | 31267.0 | 31096.0 | 31267.0 (success-conditioned, 6 of 6) |
| codex/T6/B | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 293765 | 49223.0 | 48960.833333333336 | 49223.0 (success-conditioned, 6 of 6) |
| codex/T6/C | 6 | 6 | 6 | 6 | 6 | 0 | 0 | resolved | 203413 | 31383.0 | 33902.166666666664 | 31383.0 (success-conditioned, 6 of 6) |

## Per-category failure-inclusive medians

| Cell | uncached_input | cache_write | cache_read | output |
|---|---|---|---|---|
| claude/T1/A | 66.0 | 12100.0 | 73029.5 | 1307.5 |
| claude/T1/B | 114.0 | 16187.0 | 90645.0 | 2295.0 |
| claude/T1/C | 66.0 | 10826.5 | 75045.5 | 1339.0 |
| claude/T2/A | 34.0 | 9669.0 | 45959.0 | 215.0 |
| claude/T2/B | 98.0 | 12117.5 | 80314.0 | 591.0 |
| claude/T2/C | 50.0 | 10446.0 | 60538.0 | 439.0 |
| claude/T3/A | 34.0 | 10565.0 | 45919.0 | 1113.0 |
| claude/T3/B | 66.0 | 10790.5 | 55542.5 | 1108.5 |
| claude/T3/C | 66.0 | 11763.0 | 75127.0 | 1515.0 |
| claude/T4/A | 34.0 | 10247.0 | 45907.0 | 573.5 |
| claude/T4/B | 66.0 | 11503.0 | 56468.5 | 666.5 |
| claude/T4/C | 34.0 | 10378.0 | 46149.0 | 605.5 |
| claude/T5/A | 34.0 | 9800.0 | 45929.0 | 463.0 |
| claude/T5/B | 66.0 | 12819.0 | 56494.0 | 1432.5 |
| claude/T5/C | 66.0 | 10931.5 | 74823.0 | 870.5 |
| claude/T6/A | 66.0 | 10182.5 | 73879.0 | 671.5 |
| claude/T6/B | 98.0 | 14845.0 | 77192.0 | 792.0 |
| claude/T6/C | 98.0 | 12048.0 | 105055.5 | 865.5 |
| codex/T1/A | 3814.0 | 0.0 | 27520.0 | 173.0 |
| codex/T1/B | 4123.0 | 0.0 | 43392.0 | 235.0 |
| codex/T1/C | 2841.0 | 0.0 | 28608.0 | 176.0 |
| codex/T2/A | 4708.0 | 0.0 | 25600.0 | 48.0 |
| codex/T2/B | 4709.0 | 0.0 | 43712.0 | 77.0 |
| codex/T2/C | 3440.0 | 0.0 | 27264.0 | 52.0 |
| codex/T3/A | 3694.0 | 0.0 | 27520.0 | 533.0 |
| codex/T3/B | 4559.0 | 0.0 | 43264.0 | 581.5 |
| codex/T3/C | 2718.0 | 0.0 | 28608.0 | 533.0 |
| codex/T4/A | 7040.5 | 0.0 | 27520.0 | 188.0 |
| codex/T4/B | 6946.5 | 0.0 | 63872.0 | 286.0 |
| codex/T4/C | 4471.5 | 0.0 | 27520.0 | 191.0 |
| codex/T5/A | 2280.0 | 0.0 | 28544.0 | 83.0 |
| codex/T5/B | 3565.0 | 0.0 | 44288.0 | 140.0 |
| codex/T5/C | 1258.0 | 0.0 | 29376.0 | 102.0 |
| codex/T6/A | 2240.5 | 0.0 | 28544.0 | 211.5 |
| codex/T6/B | 5395.0 | 0.0 | 43264.0 | 243.5 |
| codex/T6/C | 1476.0 | 0.0 | 29376.0 | 211.0 |

