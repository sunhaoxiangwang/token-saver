"""Task-appropriate correctness checkers. Expected values are derived from the fixture copy of the
pinned commit at check time. Each checker returns a dict with `verdict` in correct, partial,
incorrect, no_answer; a `score`; and counts, never names or content."""
import difflib
import os
import re

FENCE = re.compile(r"```[^\n]*\n(.*?)```", re.S)
# Bumped whenever a checker's verdict rule changes; recorded in every attempt record and report.
CHECKER_VERSION = 3


def _answer_or_none(answer):
    if not isinstance(answer, str) or not answer.strip():
        return None
    return answer


def _largest_fenced_block(answer):
    blocks = FENCE.findall(answer)
    if not blocks:
        return answer
    return max(blocks, key=len)


def _normalize_lines(text):
    lines = [l.rstrip() for l in text.replace("\r\n", "\n").replace("\r", "\n").split("\n")]
    while lines and lines[0] == "":
        lines.pop(0)
    while lines and lines[-1] == "":
        lines.pop()
    return lines


def _exact_lines(answer, expected_lines):
    got = _normalize_lines(_largest_fenced_block(answer))
    if got == expected_lines:
        return {"verdict": "correct", "score": 1.0, "expected_lines": len(expected_lines),
                "matching_lines": len(expected_lines)}
    sm = difflib.SequenceMatcher(a=expected_lines, b=got, autojunk=False)
    matching = sum(b.size for b in sm.get_matching_blocks())
    verdict = "partial" if matching > 0 else "incorrect"
    return {"verdict": verdict, "score": matching / max(1, len(expected_lines)),
            "expected_lines": len(expected_lines), "matching_lines": matching, "got_lines": len(got)}


def _set_compare(claimed, expected):
    claimed, expected = set(claimed), set(expected)
    inter = claimed & expected
    if claimed == expected and expected:
        return {"verdict": "correct", "score": 1.0, "expected": len(expected), "claimed": len(claimed), "matched": len(inter)}
    verdict = "partial" if inter else "incorrect"
    return {"verdict": verdict, "score": len(inter) / max(1, len(expected | claimed)),
            "expected": len(expected), "claimed": len(claimed), "matched": len(inter)}


# ----- expected values from the fixture -------------------------------------------------------

def expected_top_level(fixture):
    return sorted(os.listdir(fixture))


def expected_test_functions(fixture, rel="tests/wire_errors.rs"):
    src = open(os.path.join(fixture, rel), encoding="utf-8").read()
    names = []
    pending = False
    for line in src.split("\n"):
        s = line.strip()
        if s.startswith("#[test]"):
            pending = True
            continue
        if pending:
            m = re.match(r"(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)", s)
            if m:
                names.append(m.group(1))
                pending = False
            elif s.startswith("#["):
                continue
            else:
                pending = False
    return sorted(names)


def expected_docs_mentioning(fixture, needle="outbound_too_large"):
    found = []
    base = os.path.join(fixture, "docs")
    for dirpath, _, files in os.walk(base):
        for f in files:
            p = os.path.join(dirpath, f)
            try:
                data = open(p, "rb").read()
            except OSError:
                continue
            if needle.encode() in data:
                found.append(os.path.relpath(p, fixture))
    return sorted(found)


def expected_main_lines(fixture, n=60, rel="src/main.rs"):
    lines = open(os.path.join(fixture, rel), encoding="utf-8").read().split("\n")[:n]
    return _normalize_lines("\n".join(lines))


def expected_budget_table(fixture, rel="docs/transport.md"):
    lines = open(os.path.join(fixture, rel), encoding="utf-8").read().split("\n")
    rows = []
    in_section = False
    for l in lines:
        if l.startswith("## Budgets"):
            in_section = True
            continue
        if in_section:
            if l.startswith("|"):
                rows.append(l.rstrip())
            elif rows:
                break
    return rows


# ----- checkers -------------------------------------------------------------------------------

def check_t1(answer, fixture):
    a = _answer_or_none(answer)
    if a is None:
        return {"verdict": "no_answer", "score": 0.0}
    expected = set(expected_top_level(fixture))
    claimed = set()
    for line in a.split("\n"):
        s = line.strip()
        s = re.sub(r"^[-*+]\s+|^\d+[.)]\s+", "", s)
        s = s.strip("`*_ ")
        token = re.split(r"\s+[—–-]+\s+|\s*:\s+|\s{2,}|\s*\(", s, maxsplit=1)[0] if s else ""
        token = token.strip("`*_ ").rstrip("/")
        if token and " " not in token and len(token) <= 64:
            claimed.add(token)
    inter = claimed & expected
    precision = len(inter) / len(claimed) if claimed else 0.0
    recall = len(inter) / len(expected) if expected else 0.0
    verdict = "correct" if precision >= 0.9 and recall >= 0.9 else ("partial" if inter else "incorrect")
    return {"verdict": verdict, "score": min(precision, recall), "precision": round(precision, 3),
            "recall": round(recall, 3), "expected": len(expected), "claimed": len(claimed)}


T2_VALUE = 16384
T2_PATH = "src/lib.rs"
_NUM = re.compile(r"(?<![\w.])(\d[\d_,]*\d|\d)(?![\w.])")
_RS_PATH = re.compile(r"(?<![\w/])((?:[\w.-]+/)*[\w.-]+\.rs)\b")


def _t2_int(token):
    try:
        return int(token.replace("_", "").replace(",", ""))
    except ValueError:
        return None


def check_t2(answer, fixture):
    """Validates what the answer asserts, not what it mentions. Every decimal number of at least
    1,000 and every `.rs` path in the answer count as asserted; the answer is correct only when each
    set is a singleton holding the right value, so a contradictory or hedged answer that also
    contains the expected substrings is incorrect."""
    a = _answer_or_none(answer)
    if a is None:
        return {"verdict": "no_answer", "score": 0.0}
    body = a.replace("`", " ")
    values = {n for n in (_t2_int(v) for v in _NUM.findall(body)) if n is not None and n >= 1000}
    paths = set(_RS_PATH.findall(body))
    if not values and not paths:
        return {"verdict": "incorrect", "score": 0.0, "reason": "no value or path asserted"}
    if len(values) > 1 or len(paths) > 1:
        return {"verdict": "incorrect", "score": 0.0, "reason": "contradictory or ambiguous assertion",
                "asserted_values": len(values), "asserted_paths": len(paths)}
    value_ok = values == {T2_VALUE}
    path_ok = paths == {T2_PATH}
    if value_ok and path_ok:
        return {"verdict": "correct", "score": 1.0}
    if (value_ok and not paths) or (path_ok and not values):
        return {"verdict": "partial", "score": 0.5, "value": value_ok, "path": path_ok,
                "reason": "only one of value and path asserted"}
    return {"verdict": "incorrect", "score": 0.0, "value": value_ok, "path": path_ok}


def check_t3(answer, fixture):
    a = _answer_or_none(answer)
    if a is None:
        return {"verdict": "no_answer", "score": 0.0}
    return _exact_lines(a, expected_main_lines(fixture))


def check_t4(answer, fixture):
    a = _answer_or_none(answer)
    if a is None:
        return {"verdict": "no_answer", "score": 0.0}
    expected = expected_test_functions(fixture)
    # One name per line was asked for: a candidate is a line that is nothing but an identifier,
    # optionally as a list item, in backticks, prefixed by `fn `, or suffixed by `()`.
    claimed = set()
    for line in a.split("\n"):
        s = line.strip()
        s = re.sub(r"^[-*+]\s+|^\d+[.)]\s+", "", s).strip("`* ")
        s = re.sub(r"^fn\s+", "", s)
        s = re.sub(r"\(\)\s*;?$", "", s).strip("`* ")
        if re.fullmatch(r"[a-z][a-z0-9_]*", s):
            claimed.add(s)
    return _set_compare(claimed, set(expected))


def check_t5(answer, fixture):
    a = _answer_or_none(answer)
    if a is None:
        return {"verdict": "no_answer", "score": 0.0}
    expected = expected_docs_mentioning(fixture)
    # Any file below docs/ may match, whatever its extension (the fixture holds .md, .json, .jsonl).
    claimed = set(m.rstrip(".,;:)") for m in re.findall(r"(?<![\w/])docs/[A-Za-z0-9_./-]+\.[A-Za-z0-9]+", a))
    return _set_compare(claimed, expected)


def check_t6(answer, fixture):
    a = _answer_or_none(answer)
    if a is None:
        return {"verdict": "no_answer", "score": 0.0}
    expected = expected_budget_table(fixture)
    got = [l.rstrip() for l in a.replace("\r\n", "\n").split("\n") if l.strip().startswith("|")]
    def norm(row):
        return [c.strip() for c in row.strip().strip("|").split("|")]
    exp_n = [norm(r) for r in expected]
    got_n = [norm(r) for r in got]
    if got_n == exp_n:
        return {"verdict": "correct", "score": 1.0, "rows": len(expected)}
    sm = difflib.SequenceMatcher(a=[tuple(r) for r in exp_n], b=[tuple(r) for r in got_n], autojunk=False)
    matching = sum(b.size for b in sm.get_matching_blocks())
    verdict = "partial" if matching > 0 else "incorrect"
    return {"verdict": verdict, "score": matching / max(1, len(exp_n)), "rows": len(exp_n), "matching_rows": matching}


CHECKERS = {"T1": check_t1, "T2": check_t2, "T3": check_t3, "T4": check_t4, "T5": check_t5, "T6": check_t6}


def check(task, answer, fixture):
    if task not in CHECKERS:
        a = _answer_or_none(answer)
        return {"verdict": "not_applicable" if a is not None else "no_answer", "score": 0.0}
    return CHECKERS[task](answer, fixture)
