"""Per-cell aggregation with failure-inclusive denominators and the pre-registered claim rule
(TS-M1-PLAN-001 revision 4, section 8). No percentage, no cost."""
import json
import statistics

CATEGORIES = ["uncached_input", "cache_write", "cache_read", "output"]
MEASURED_ATTEMPTS = [1, 2, 3, 4, 5, 6]


def _median(values):
    values = [v for v in values if v is not None]
    return statistics.median(values) if values else None


def attempt_set_check(attempts, expected=MEASURED_ATTEMPTS):
    """Measured attempt ids must be exactly `expected`, each once. Returns (complete, missing,
    duplicates)."""
    ids = [a["attempt"] for a in attempts]
    missing = [i for i in expected if i not in ids]
    seen = set()
    duplicates = sorted({i for i in ids if i in seen or seen.add(i)})
    extra = sorted({i for i in ids if i not in expected})
    complete = not missing and not duplicates and not extra
    return complete, missing, duplicates + extra


def usage_trusted(a):
    """A failed attempt that reports zero tokens in every category did not measure anything (the
    client failed before or without a billed request); its usage is treated as unavailable."""
    if a["usage"] is None:
        return False
    if not a["completed"] and (a.get("total_tokens") or 0) == 0:
        return False
    return True


def cell_summary(attempts):
    """`attempts` are the measured attempts of one cell (attempt 0, the warm-up, excluded). A cell
    is `resolved` only when attempts 1 to 6 are present exactly once and every one has trusted
    usage."""
    n = len(attempts)
    set_complete, missing_ids, bad_ids = attempt_set_check(attempts)
    completed = [a for a in attempts if a["completed"]]
    compliant = [a for a in attempts if a["compliance"]["status"] == "compliant"]
    attributed = [a for a in attempts if a["attribution"] in ("verified", "not applicable")]
    correct = [a for a in attempts if a["correctness"]["verdict"] == "correct"]
    partial = [a for a in attempts if a["correctness"]["verdict"] == "partial"]
    usage_missing = [a for a in attempts if not usage_trusted(a)]
    resolved = set_complete and not usage_missing
    sums = {}
    medians = {}
    for cat in CATEGORIES + ["total_tokens"]:
        vals = []
        for a in attempts:
            if not usage_trusted(a):
                continue
            vals.append(a["total_tokens"] if cat == "total_tokens" else (a["usage"].get(cat) or 0))
        sums[cat] = sum(vals) if vals else None
        medians[cat] = _median(vals)
    def per_correct(cat):
        if not correct or sums[cat] is None:
            return None
        return sums[cat] / len(correct)
    eligible = [a for a in attempts if a in correct and a in compliant and a in attributed and usage_trusted(a)]
    success_medians = {}
    for cat in CATEGORIES + ["total_tokens"]:
        vals = [a["total_tokens"] if cat == "total_tokens" else (a["usage"].get(cat) or 0) for a in eligible]
        success_medians[cat] = _median(vals)
    return {
        "attempts": n,
        "attempt_ids": sorted(a["attempt"] for a in attempts),
        "attempt_set_complete": set_complete,
        "missing_attempt_ids": missing_ids,
        "duplicate_or_extra_attempt_ids": bad_ids,
        "completed": len(completed),
        "server_calls_min": min((len(a["tool_calls"]) for a in attempts), default=0),
        "attempts_without_server_calls": sum(1 for a in attempts if not a["tool_calls"]),
        "compliant": len(compliant),
        "attributed": len(attributed),
        "correct": len(correct),
        "partial": len(partial),
        "usage_unavailable": len(usage_missing),
        "resolution": "resolved" if resolved else "unresolved",
        "failure_inclusive_sum": sums,
        "failure_inclusive_median": medians,
        "total_tokens_per_correct_answer": per_correct("total_tokens") if correct else "no correct answers",
        "per_category_per_correct_answer": {c: per_correct(c) for c in CATEGORIES} if correct else "no correct answers",
        "success_conditioned_median": success_medians,
        "success_conditioned_label": "success-conditioned, %d of %d" % (len(eligible), n),
        "lower_bounds_only": not resolved,
        "bytes": [{"attempt": a["attempt"],
                   "client_recorded_result_bytes": sum((c.get("client_recorded_result_bytes") or 0) for c in a["tool_calls"]),
                   "relay_result_text_bytes": sum((c.get("relay_result_text_bytes") or 0) for c in a["tool_calls"]),
                   "relay_response_frame_bytes": sum((c.get("relay_response_frame_bytes") or 0) for c in a["tool_calls"]),
                   "tool_calls": len(a["tool_calls"]),
                   "native_tool_uses": a.get("native_tool_uses", 0)} for a in attempts],
    }


def claim(cell_a, cell_b):
    """The pre-registered rule for arm B against arm A. Returns (verdict, reasons)."""
    reasons = []
    for name, cell in (("A", cell_a), ("B", cell_b)):
        if not cell.get("attempt_set_complete", False):
            reasons.append("arm %s attempt set incomplete: missing %s, duplicate or extra %s" % (
                name, cell.get("missing_attempt_ids"), cell.get("duplicate_or_extra_attempt_ids")))
        if cell["usage_unavailable"]:
            reasons.append("arm %s has %d attempt(s) with usage unavailable" % (name, cell["usage_unavailable"]))
    if reasons:
        return "unresolved", reasons
    if cell_b["compliant"] != cell_b["attempts"] or cell_b["attributed"] != cell_b["attempts"]:
        reasons.append("arm B has noncompliant or unverified attempts")
    if cell_b.get("attempts_without_server_calls", 0) > 0:
        # Nothing can be attributed to the server in an attempt that never called it.
        reasons.append("arm B has %d attempt(s) without any token-saver call" % cell_b["attempts_without_server_calls"])
    if cell_b["correct"] < cell_a["correct"]:
        reasons.append("arm B correct count below arm A")
    pa, pb = cell_a["total_tokens_per_correct_answer"], cell_b["total_tokens_per_correct_answer"]
    if isinstance(pa, str) or isinstance(pb, str):
        reasons.append("no correct answers in an arm")
    elif not pb < pa:
        reasons.append("arm B total tokens per correct answer not lower")
    ma, mb = cell_a["success_conditioned_median"]["total_tokens"], cell_b["success_conditioned_median"]["total_tokens"]
    if ma is None or mb is None or not mb < ma:
        reasons.append("success-conditioned medians do not point the same way")
    if reasons:
        return "no saving established", reasons
    return "fewer total tokens per correct answer", []


def category_deltas(cell_a, cell_b):
    out = {}
    for c in CATEGORIES + ["total_tokens"]:
        a = cell_a["failure_inclusive_median"].get(c)
        b = cell_b["failure_inclusive_median"].get(c)
        out[c] = None if a is None or b is None else b - a
    return out


def build_report(records):
    """records: list of attempt records (all attempts including warm-ups)."""
    cells = {}
    for r in records:
        key = (r["client"], r["task"], r["arm"])
        cells.setdefault(key, []).append(r)
    report = {"cells": {}, "comparisons": {}, "warmups": {}}
    for key, atts in sorted(cells.items()):
        measured = sorted([a for a in atts if a["attempt"] > 0], key=lambda a: a["attempt"])
        warm = [a for a in atts if a["attempt"] == 0]
        name = "%s/%s/%s" % key
        report["cells"][name] = cell_summary(measured)
        report["warmups"][name] = [{"attempt": a["attempt"], "total_tokens": a["total_tokens"],
                                   "correctness": a["correctness"]["verdict"]} for a in warm]
    clients = sorted({k[0] for k in cells})
    tasks = sorted({k[1] for k in cells})
    for client in clients:
        for task in tasks:
            a = report["cells"].get("%s/%s/A" % (client, task))
            b = report["cells"].get("%s/%s/B" % (client, task))
            c = report["cells"].get("%s/%s/C" % (client, task))
            if a and b:
                verdict, reasons = claim(a, b)
                report["comparisons"]["%s/%s" % (client, task)] = {
                    "arm_B_vs_A": {"verdict": verdict, "reasons": reasons, "median_deltas": category_deltas(a, b)},
                    "arm_C_vs_A": ({"verdict": claim(a, c)[0], "reasons": claim(a, c)[1], "median_deltas": category_deltas(a, c)} if c else None),
                }
    return report


def render_markdown(report, title="Milestone 1 comparison report"):
    out = [f"# {title}", "", "Every number is a token count or a byte count; nothing is a cost or a percentage.",
           "Cells report all six measured attempts (failure-inclusive) with denominators; the warm-up attempt is excluded.",
           "A cell is resolved only when attempts 1 to 6 are present exactly once and every one has usage.", ""]
    if report.get("notes"):
        out.append("## Notes")
        out.append("")
        for n in report["notes"]:
            out.append("- " + n)
        out.append("")
    if report.get("checker_version") is not None:
        out.append("Checker version: %s" % report["checker_version"])
        out.append("")
    out.append("## Claim rule outcomes (arm B versus arm A)")
    out.append("")
    out.append("| Client | Task | Verdict | Reasons | total_tokens median delta (B - A) |")
    out.append("|---|---|---|---|---|")
    for key, cmp in sorted(report["comparisons"].items()):
        client, task = key.split("/")
        v = cmp["arm_B_vs_A"]
        out.append("| %s | %s | %s | %s | %s |" % (client, task, v["verdict"], "; ".join(v["reasons"]) or "-", v["median_deltas"].get("total_tokens")))
    out.append("")
    out.append("## Cells")
    out.append("")
    out.append("| Cell | n | completed | compliant | attributed | correct | partial | usage unavailable | resolution | sum total_tokens | median total_tokens | total_tokens per correct answer | success-conditioned median total_tokens |")
    out.append("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for name, c in sorted(report["cells"].items()):
        out.append("| %s | %d | %d | %d | %d | %d | %d | %d | %s | %s | %s | %s | %s (%s) |" % (
            name, c["attempts"], c["completed"], c["compliant"], c["attributed"], c["correct"], c["partial"],
            c["usage_unavailable"], c["resolution"], c["failure_inclusive_sum"].get("total_tokens"),
            c["failure_inclusive_median"].get("total_tokens"), c["total_tokens_per_correct_answer"],
            c["success_conditioned_median"].get("total_tokens"), c["success_conditioned_label"]))
    out.append("")
    out.append("## Per-category failure-inclusive medians")
    out.append("")
    out.append("| Cell | uncached_input | cache_write | cache_read | output |")
    out.append("|---|---|---|---|---|")
    for name, c in sorted(report["cells"].items()):
        m = c["failure_inclusive_median"]
        out.append("| %s | %s | %s | %s | %s |" % (name, m.get("uncached_input"), m.get("cache_write"), m.get("cache_read"), m.get("output")))
    out.append("")
    return "\n".join(out) + "\n"


def dumps(report):
    return json.dumps(report, indent=1, sort_keys=True)
