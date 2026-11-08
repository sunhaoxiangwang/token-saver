"""Harness tests: canonical vectors, synthetic claim-rule cases (TS-M1-REVIEW-003 F04), checker
negatives and partials, correlation cases, and the retention pipeline with canaries."""
import json
import os
import shutil
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from tokcmp import canonical, checkers, extract, fixture, report, retention  # noqa: E402


def attempt(client, task, arm, n, total, correct=True, compliant=True, attribution="verified",
            usage=None, completed=True, cache_read=0, uncached=None, calls=1):
    if usage is None and (total is not None or uncached is not None):
        uncached = total if uncached is None else uncached
        usage = {"uncached_input": uncached, "cache_write": 0, "cache_read": cache_read, "output": 0}
        total = uncached + cache_read
    return {"client": client, "task": task, "arm": arm, "attempt": n, "completed": completed,
            "compliance": {"status": "compliant" if compliant else "noncompliant", "reasons": []},
            "attribution": attribution,
            "correctness": {"verdict": "correct" if correct else "incorrect"},
            "usage": usage, "total_tokens": total if usage is not None else None,
            "tool_calls": [{"tool": "file_slice"}] * (calls if arm != "A" else 0), "native_tool_uses": 0}


class CanonicalVectors(unittest.TestCase):
    VECTORS = [
        ({}, 2, "44136fa355b3678a"),
        ({"path": ""}, 11, "518341a7f82a2945"),
        ({"max_lines": 60, "path": "src/main.rs", "start_line": 1}, 52, "addece581e7e5c6f"),
        ({"path": "src/main.rs", "start_line": 1, "max_lines": 60}, 52, "addece581e7e5c6f"),
        ({"case_sensitive": False, "context_lines": 0, "query": "outbound_too_large"}, 71, "22d8cd22e833ca09"),
        ({"max_bytes": 256, "path": "docs/été/日本.md"}, 47, "78bbd623f9a8a6d3"),
        ({"max_matches": 100, "query": "tab\there \"quoted\" back\\slash \u0001ctl"}, 72, "22d9e5e171640ea5"),
        ({"depth": 3, "include_hidden": True, "max_entries": 200, "path": "a"}, 62, "096afd0e9fb660cb"),
        ({"b": {"z": [1, 2, {"y": None}], "a": "x"}, "A": "upper sorts first", "é": "non-ascii key"}, 81, "77c4c3da126e8539"),
    ]

    def test_vectors(self):
        for args, size, digest in self.VECTORS:
            d, n = canonical.args_digest(args)
            self.assertEqual((d, n), (digest, size), args)
        self.assertEqual(canonical.text_digest("")[0], "e3b0c44298fc1c14")
        self.assertEqual(canonical.text_digest("d .\nf README.md 1024\n")[0], "023dfd9f7a5c3f0d")
        self.assertEqual(canonical.args_digest({"start_line": 1.5}), (None, None))


class ClaimRule(unittest.TestCase):
    def cells(self, a_atts, b_atts):
        return report.cell_summary(a_atts), report.cell_summary(b_atts)

    def test_retries_cannot_reverse_savings(self):
        a = [attempt("claude", "T1", "A", i, 1000) for i in range(1, 7)]
        b = [attempt("claude", "T1", "B", i, 800) for i in range(1, 4)] + \
            [attempt("claude", "T1", "B", i, 1200, correct=False) for i in range(4, 7)]
        ca, cb = self.cells(a, b)
        self.assertLess(cb["success_conditioned_median"]["total_tokens"], ca["success_conditioned_median"]["total_tokens"])
        self.assertEqual(cb["total_tokens_per_correct_answer"], 6000 / 3)
        self.assertEqual(ca["total_tokens_per_correct_answer"], 1000)
        verdict, reasons = report.claim(ca, cb)
        self.assertEqual(verdict, "no saving established")
        self.assertTrue(any("correct count" in r or "per correct answer" in r for r in reasons))

    def test_cached_usage_shift_is_not_a_saving(self):
        a = [attempt("claude", "T2", "A", i, None, uncached=1000, cache_read=5000) for i in range(1, 7)]
        b = [attempt("claude", "T2", "B", i, None, uncached=500, cache_read=7000) for i in range(1, 7)]
        ca, cb = self.cells(a, b)
        self.assertLess(cb["failure_inclusive_median"]["uncached_input"], ca["failure_inclusive_median"]["uncached_input"])
        self.assertGreater(cb["failure_inclusive_median"]["total_tokens"], ca["failure_inclusive_median"]["total_tokens"])
        self.assertEqual(report.claim(ca, cb)[0], "no saving established")

    def test_unknown_usage_blocks_any_claim(self):
        a = [attempt("codex", "T3", "A", i, 1000) for i in range(1, 7)]
        b = [attempt("codex", "T3", "B", i, 500) for i in range(1, 6)] + \
            [attempt("codex", "T3", "B", 6, None, completed=False, correct=False)]
        ca, cb = self.cells(a, b)
        self.assertEqual(cb["resolution"], "unresolved")
        self.assertTrue(cb["lower_bounds_only"])
        self.assertEqual(report.claim(ca, cb)[0], "unresolved")

    def test_failed_attempt_with_zero_tokens_is_usage_unavailable(self):
        a = [attempt("claude", "T6", "A", i, 1000) for i in range(1, 7)]
        b = [attempt("claude", "T6", "B", i, 600) for i in range(1, 6)] + \
            [attempt("claude", "T6", "B", 6, None, completed=False, correct=False,
                     usage={"uncached_input": 0, "cache_write": 0, "cache_read": 0, "output": 0})]
        b[-1]["total_tokens"] = 0
        ca, cb = self.cells(a, b)
        self.assertEqual(cb["usage_unavailable"], 1)
        self.assertEqual(cb["resolution"], "unresolved")
        self.assertEqual(report.claim(ca, cb)[0], "unresolved")
        # A failed attempt that did consume tokens keeps its usage in the failure-inclusive totals.
        b[-1]["usage"] = {"uncached_input": 50, "cache_write": 0, "cache_read": 0, "output": 0}
        b[-1]["total_tokens"] = 50
        cb2 = report.cell_summary(b)
        self.assertEqual(cb2["usage_unavailable"], 0)
        self.assertEqual(cb2["failure_inclusive_sum"]["total_tokens"], 3050)

    def test_foreign_mcp_and_unverified_attribution_refuse_the_claim_but_keep_usage(self):
        a = [attempt("codex", "T4", "A", i, 1000) for i in range(1, 7)]
        b = [attempt("codex", "T4", "B", i, 500) for i in range(1, 6)] + \
            [attempt("codex", "T4", "B", 6, 500, attribution="refused (foreign server or tool)", compliant=False)]
        ca, cb = self.cells(a, b)
        self.assertEqual(cb["failure_inclusive_sum"]["total_tokens"], 3000)
        verdict, reasons = report.claim(ca, cb)
        self.assertEqual(verdict, "no saving established")
        self.assertIn("arm B has noncompliant or unverified attempts", reasons)
        b2 = [attempt("codex", "T4", "B", i, 500) for i in range(1, 6)] + \
             [attempt("codex", "T4", "B", 6, 500, attribution="unverified")]
        self.assertEqual(report.claim(ca, report.cell_summary(b2))[0], "no saving established")

    def test_incomplete_attempt_sets_block_any_claim(self):
        # TS-M1-REVIEW (harness) F01: one measured attempt per arm.
        a = [attempt("claude", "T6", "A", 1, 1000)]
        b = [attempt("claude", "T6", "B", 1, 600)]
        ca, cb = self.cells(a, b)
        self.assertFalse(ca["attempt_set_complete"])
        self.assertEqual(ca["missing_attempt_ids"], [2, 3, 4, 5, 6])
        self.assertEqual(ca["resolution"], "unresolved")
        verdict, reasons = report.claim(ca, cb)
        self.assertEqual(verdict, "unresolved")
        self.assertTrue(any("attempt set incomplete" in r for r in reasons))
        # Six records with a duplicate attempt 5 and no attempt 6.
        a = [attempt("claude", "T6", "A", i, 1000) for i in range(1, 7)]
        b = [attempt("claude", "T6", "B", i, 600) for i in (1, 2, 3, 4, 5, 5)]
        ca, cb = self.cells(a, b)
        self.assertEqual(cb["attempts"], 6)
        self.assertFalse(cb["attempt_set_complete"])
        self.assertEqual(cb["missing_attempt_ids"], [6])
        self.assertEqual(cb["duplicate_or_extra_attempt_ids"], [5])
        self.assertEqual(report.claim(ca, cb)[0], "unresolved")
        # An extra attempt id outside 1..6 also blocks.
        b = [attempt("claude", "T6", "B", i, 600) for i in range(1, 7)] + [attempt("claude", "T6", "B", 7, 600)]
        self.assertEqual(report.claim(ca, report.cell_summary(b))[0], "unresolved")
        # Exactly 1..6 once each resolves.
        b = [attempt("claude", "T6", "B", i, 600) for i in range(1, 7)]
        self.assertEqual(report.claim(ca, report.cell_summary(b))[0], "fewer total tokens per correct answer")

    def test_an_arm_attempt_without_server_calls_cannot_support_a_claim(self):
        a = [attempt("codex", "T4", "A", i, 1000) for i in range(1, 7)]
        c = [attempt("codex", "T4", "C", i, 600, calls=0) for i in range(1, 7)]
        ca, cc = self.cells(a, c)
        self.assertEqual(cc["attempts_without_server_calls"], 6)
        verdict, reasons = report.claim(ca, cc)
        self.assertEqual(verdict, "no saving established")
        self.assertTrue(any("without any token-saver call" in r for r in reasons))
        c2 = [attempt("codex", "T4", "C", i, 600, calls=1) for i in range(1, 7)]
        self.assertEqual(report.claim(ca, report.cell_summary(c2))[0], "fewer total tokens per correct answer")

    def test_positive_case_names_the_measure(self):
        a = [attempt("claude", "T5", "A", i, 1000) for i in range(1, 7)]
        b = [attempt("claude", "T5", "B", i, 600) for i in range(1, 7)]
        ca, cb = self.cells(a, b)
        verdict, reasons = report.claim(ca, cb)
        self.assertEqual(verdict, "fewer total tokens per correct answer")
        self.assertEqual(reasons, [])
        md = report.render_markdown(report.build_report(a + b))
        self.assertIn("fewer total tokens per correct answer", md)
        self.assertNotIn("%", md.split("## Cells")[0].split("outcomes")[1])


class Compliance(unittest.TestCase):
    def test_foreign_server_in_arm_b_is_noncompliant(self):
        parsed = {"mcp_calls": [{"server": "other", "tool": "read", "args_sha256": "x"}], "native_tools": []}
        relay = {"present": True, "complete": True, "any_traffic": True, "requests": []}
        status, reasons = extract.compliance("codex", "B", parsed, relay)
        self.assertEqual(status, "noncompliant")
        self.assertTrue(any("another server" in r for r in reasons))
        parsed_c = {"mcp_calls": [], "native_tools": ["mcp__other__read"]}
        status, _ = extract.compliance("claude", "B", parsed_c, relay)
        self.assertEqual(status, "noncompliant")
        self.assertEqual(extract.attribution("B", parsed, {"status": "complete"}), "refused (foreign server or tool)")

    def test_arm_a_with_mcp_or_relay_traffic_is_noncompliant(self):
        parsed = {"mcp_calls": [], "native_tools": ["Read"]}
        relay = {"present": True, "complete": True, "any_traffic": True, "requests": []}
        self.assertEqual(extract.compliance("claude", "A", parsed, relay)[0], "noncompliant")
        relay2 = {"present": False, "complete": None, "any_traffic": False, "requests": []}
        self.assertEqual(extract.compliance("claude", "A", parsed, relay2)[0], "compliant")

    def test_tool_search_is_allowed_in_arm_b_for_claude(self):
        parsed = {"mcp_calls": [{"server": "token-saver", "tool": "file_slice"}], "native_tools": ["ToolSearch"]}
        relay = {"present": True, "complete": True, "any_traffic": True, "requests": []}
        self.assertEqual(extract.compliance("claude", "B", parsed, relay)[0], "compliant")


class Correlation(unittest.TestCase):
    def relay(self, requests, complete=True):
        return {"present": True, "complete": complete, "reason": None, "requests": requests, "any_traffic": True}

    def test_error_only_run_correlates_with_null_digests(self):
        client = [{"tool": "file_slice", "args_sha256": "a1", "result_sha256": "e1", "is_error": True},
                  {"tool": "file_slice", "args_sha256": "a2", "result_sha256": None, "is_error": True, "protocol_error": True}]
        relay = self.relay([
            {"tool": "file_slice", "args_sha256": "a1", "response": {"result_sha256": "e1", "is_error": True}},
            {"tool": "file_slice", "args_sha256": "a2", "response": {"error_code": -32602, "result_sha256": None}}])
        self.assertEqual(extract.correlate(client, relay)["status"], "complete")

    def test_repeated_identical_calls_match_by_order(self):
        client = [{"tool": "workspace_outline", "args_sha256": "o", "result_sha256": "r"}] * 3
        relay = self.relay([{"tool": "workspace_outline", "args_sha256": "o", "response": {"result_sha256": "r"}}] * 3)
        c = extract.correlate(client, relay)
        self.assertEqual(c["status"], "complete")
        self.assertEqual(c["repeated_calls"], 3)
        self.assertIn("not individually distinguishable", c["note"])

    def test_mismatches_are_incomplete_never_corroborated(self):
        client = [{"tool": "workspace_outline", "args_sha256": "o", "result_sha256": "r"}]
        relay = self.relay([{"tool": "workspace_outline", "args_sha256": "o", "response": {"result_sha256": "r"}}] * 2)
        self.assertEqual(extract.correlate(client, relay)["status"], "incomplete")
        relay2 = self.relay([{"tool": "workspace_outline", "args_sha256": "o", "response": {"result_sha256": "DIFFERENT"}}])
        self.assertEqual(extract.correlate(client, relay2)["reason"], "result digest differs")
        relay3 = self.relay([{"tool": "workspace_outline", "args_sha256": "o", "response": {"result_sha256": "r"}}], complete=False)
        self.assertEqual(extract.correlate(client, relay3)["status"], "incomplete")
        self.assertEqual(extract.attribution("B", {"mcp_calls": [{"server": "token-saver", "tool": "file_slice"}]}, {"status": "incomplete"}), "unverified")


class Checkers(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.mkdtemp(prefix="tokcmp-")
        cls.fx = os.path.join(cls.tmp, "fx")
        os.makedirs(os.path.join(cls.fx, "src"))
        os.makedirs(os.path.join(cls.fx, "docs"))
        os.makedirs(os.path.join(cls.fx, "tests"))
        with open(os.path.join(cls.fx, "src", "main.rs"), "w") as f:
            f.write("".join("line %d\n" % i for i in range(1, 80)))
        with open(os.path.join(cls.fx, "src", "lib.rs"), "w") as f:
            f.write("pub const MAX_OUTBOUND_FRAME_BYTES: usize = 16_384;\n")
        with open(os.path.join(cls.fx, "tests", "wire_errors.rs"), "w") as f:
            f.write("#[test]\nfn alpha_one() {}\n#[test]\nfn beta_two() {}\nfn helper() {}\n")
        with open(os.path.join(cls.fx, "docs", "a.md"), "w") as f:
            f.write("mentions outbound_too_large here\n")
        with open(os.path.join(cls.fx, "docs", "b.md"), "w") as f:
            f.write("nothing\n")
        os.makedirs(os.path.join(cls.fx, "docs", "evidence"))
        with open(os.path.join(cls.fx, "docs", "evidence", "index.json"), "w") as f:
            f.write('{"note": "fatal cause=outbound_too_large"}\n')
        with open(os.path.join(cls.fx, "docs", "transport.md"), "w") as f:
            f.write("## Budgets\n\n| Budget | Starts at | Enforced by |\n|---|---|---|\n| processing, 5 s | admit_at | handler |\n| delivery, 5 s | commit_at | supervisor |\n\nText after.\n")
        with open(os.path.join(cls.fx, "README.md"), "w") as f:
            f.write("x\n")

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp, ignore_errors=True)

    def test_t3_exact_reproduction_only(self):
        exact = "```rust\n" + "".join("line %d\n" % i for i in range(1, 61)) + "```"
        self.assertEqual(checkers.check("T3", exact, self.fx)["verdict"], "correct")
        partial = "```rust\n" + "".join("line %d\n" % i for i in range(1, 58)) + "```"
        r = checkers.check("T3", partial, self.fx)
        self.assertEqual(r["verdict"], "partial")
        self.assertEqual(r["matching_lines"], 57)
        self.assertEqual(checkers.check("T3", "", self.fx)["verdict"], "no_answer")

    def test_t2_validates_the_asserted_answer(self):
        c = lambda s: checkers.check("T2", s, self.fx)["verdict"]
        self.assertEqual(c("16_384 in src/lib.rs"), "correct")
        self.assertEqual(c("16384 in src/lib.rs"), "correct")
        self.assertEqual(c("`16_384` in `src/lib.rs`"), "correct")
        self.assertEqual(c("MAX_OUTBOUND_FRAME_BYTES is 16_384 and it is defined in src/lib.rs."), "correct")
        # TS-M1-REVIEW (harness) F02: a contradictory answer that contains the expected substrings.
        self.assertEqual(c("MAX_OUTBOUND_FRAME_BYTES is 32768 in src/lib.rs. The value 16384 is obsolete and is not the current value."), "incorrect")
        self.assertEqual(c("32768 in src/lib.rs (not 16384)"), "incorrect")
        self.assertEqual(c("16384 in src/transport.rs"), "incorrect")
        self.assertEqual(c("16384 in src/lib.rs or src/transport.rs"), "incorrect")
        self.assertEqual(c("65536 in src/lib.rs"), "incorrect")
        self.assertEqual(c("The limit is 16_384, defined in src/transport.rs"), "incorrect")
        self.assertEqual(c("MAX_OUTBOUND_FRAME_BYTES is 16_384"), "partial")
        self.assertEqual(c("It is defined in src/lib.rs"), "partial")
        self.assertEqual(c("I could not find it"), "incorrect")
        self.assertEqual(c(""), "no_answer")

    def test_t4_t5_set_equality(self):
        self.assertEqual(checkers.check("T4", "alpha_one\nbeta_two", self.fx)["verdict"], "correct")
        self.assertEqual(checkers.check("T4", "alpha_one\nbeta_two\nhelper_fn\ngamma_three", self.fx)["verdict"], "partial")
        self.assertEqual(checkers.check("T4", "alpha_one", self.fx)["verdict"], "partial")
        self.assertEqual(checkers.check("T4", "- `alpha_one`\n- `beta_two`", self.fx)["verdict"], "correct")
        self.assertEqual(checkers.check("T4", "fn alpha_one()\nfn beta_two()", self.fx)["verdict"], "correct")
        # Prose with underscores on the same line as a name is not a bare name line.
        self.assertEqual(checkers.check("T4", "The tests are alpha_one and beta_two in wire_errors.", self.fx)["verdict"], "incorrect")
        # Harness finding F03 (self-found): non-.md files under docs/ are part of the expected set
        # and must be claimable.
        self.assertEqual(checkers.check("T5", "docs/a.md\ndocs/evidence/index.json", self.fx)["verdict"], "correct")
        self.assertEqual(checkers.check("T5", "- `docs/a.md`\n- `docs/evidence/index.json`.", self.fx)["verdict"], "correct")
        self.assertEqual(checkers.check("T5", "docs/a.md", self.fx)["verdict"], "partial")
        self.assertEqual(checkers.check("T5", "docs/a.md\ndocs/b.md\ndocs/evidence/index.json", self.fx)["verdict"], "partial")
        # Negative: mentions the expected substring inside a wrong path.
        self.assertEqual(checkers.check("T5", "docs/other/a.md", self.fx)["verdict"], "incorrect")

    def test_t1_precision_recall(self):
        good = "- src — code\n- docs — documents\n- tests — tests\n- README.md — readme\n"
        self.assertEqual(checkers.check("T1", good, self.fx)["verdict"], "correct")
        noisy = good + "- extra_one — invented\n- extra_two — invented\n"
        self.assertEqual(checkers.check("T1", noisy, self.fx)["verdict"], "partial")

    def test_t6_rows(self):
        table = "| Budget | Starts at | Enforced by |\n|---|---|---|\n| processing, 5 s | admit_at | handler |\n| delivery, 5 s | commit_at | supervisor |"
        self.assertEqual(checkers.check("T6", table, self.fx)["verdict"], "correct")
        wrong = table.replace("supervisor", "nobody")
        self.assertEqual(checkers.check("T6", wrong, self.fx)["verdict"], "partial")


class Retention(unittest.TestCase):
    def test_canary_pipeline(self):
        tmp = tempfile.mkdtemp(prefix="tokcmp-ret-")
        try:
            raw = os.path.join(tmp, "raw")
            os.makedirs(raw)
            stream = os.path.join(raw, "stream.jsonl")
            events = [
                {"type": "system", "subtype": "init", "claude_code_version": "2.1.319", "model": "m"},
                {"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "t1", "name": "mcp__token-saver__workspace_outline", "input": {"path": ""}}]}},
                {"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t1", "content": [{"type": "text", "text": "d CANARY_DIR_7f3a\nf canary.txt 20"}]}]}},
                {"type": "result", "result": "The layout includes CANARY_DIR_7f3a", "usage": {"input_tokens": 5, "cache_creation_input_tokens": 1, "cache_read_input_tokens": 2, "output_tokens": 3}, "is_error": False, "num_turns": 2},
            ]
            with open(stream, "w") as f:
                for e in events:
                    f.write(json.dumps(e) + "\n")
            parsed = extract.parse_claude(stream)
            self.assertIn("CANARY_DIR_7f3a", open(stream).read())
            self.assertEqual(parsed["mcp_calls"][0]["tool"], "workspace_outline")
            self.assertEqual(parsed["mcp_calls"][0]["args_sha256"], "518341a7f82a2945")
            expected_digest = canonical.text_digest("d CANARY_DIR_7f3a\nf canary.txt 20")[0]
            self.assertEqual(parsed["mcp_calls"][0]["result_sha256"], expected_digest)
            retained = {k: v for k, v in parsed.items() if k not in ("answer", "tool_uses", "tool_results")}
            text = json.dumps(retained)
            forbidden = fixture.forbidden_strings("/tmp/fixture-x")
            self.assertEqual(retention.scan_bytes(text.encode(), forbidden), [])
            hashes = retention.retire_raw(raw)
            self.assertIn("stream.jsonl", hashes)
            self.assertFalse(os.path.exists(raw))
            self.assertEqual(extract.total_tokens(parsed["usage"]), 11)
            manifest = os.path.join(tmp, "run-1.manifest.json")
            with open(manifest, "w") as f:
                json.dump({"server": ["/relay", "--out", "/home/u/repo/evidence/x", "--", "/home/u/repo/bin", "--root", "/home/u/repo/evidence/x/fixture"]}, f)
            retention.sanitize_manifest(manifest, [("/home/u/repo/evidence/x/fixture", "<fixture>"), ("/home/u/repo/evidence/x", "<relay>"), ("/home/u/repo", "<repo>")])
            m = json.load(open(manifest))
            self.assertEqual(m["server"], ["/relay", "--out", "<relay>", "--", "<repo>/bin", "--root", "<fixture>"])
            self.assertTrue(m["argv_sanitized"])
            synthetic = b"contact someone" + b"@" + b"example.com"
            self.assertEqual(retention.scan_bytes(synthetic, []), ["e-mail address present"])
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_codex_parse_and_usage(self):
        tmp = tempfile.mkdtemp(prefix="tokcmp-codex-")
        try:
            stream = os.path.join(tmp, "s.jsonl")
            events = [
                {"type": "thread.started"},
                {"type": "item.completed", "item": {"type": "command_execution", "command": "ls"}},
                {"type": "item.completed", "item": {"type": "mcp_tool_call", "server": "token-saver", "tool": "text_search", "arguments": {"query": "x"}, "result": {"content": [{"type": "text", "text": "== a\n1:x"}], "isError": False}, "status": "completed", "error": None}},
                {"type": "item.completed", "item": {"type": "agent_message", "text": "answer"}},
                {"type": "turn.completed", "usage": {"input_tokens": 100, "cached_input_tokens": 60, "cache_write_input_tokens": 0, "output_tokens": 10, "reasoning_output_tokens": 4}},
                {"type": "turn.completed", "usage": {"input_tokens": 50, "cached_input_tokens": 50, "cache_write_input_tokens": 0, "output_tokens": 5, "reasoning_output_tokens": 0}},
            ]
            with open(stream, "w") as f:
                for e in events:
                    f.write(json.dumps(e) + "\n")
            p = extract.parse_codex(stream)
            self.assertEqual(p["usage"]["uncached_input"], 40)
            self.assertEqual(p["usage"]["cache_read"], 110)
            self.assertEqual(extract.total_tokens(p["usage"]), 165)
            self.assertEqual(p["native_tools"], ["command_execution"])
            self.assertEqual(p["answer"], "answer")
            relay = {"present": True, "complete": True, "any_traffic": True, "requests": []}
            self.assertEqual(extract.compliance("codex", "B", p, relay)[0], "noncompliant")
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()
