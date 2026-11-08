#!/usr/bin/env python3 -I
"""token-compare: run, extract, and report the Milestone 1 comparison.

usage:
  python3 -I compare.py run --repo <repo> --commit <sha> --out <runs dir> [--client claude|codex]
                            [--tasks T1,T2] [--arms A,B,C] [--attempts 7] [--config compare.json]
  python3 -I compare.py report --runs <runs dir> --out <report dir>

`run` is resumable: an attempt whose record exists is skipped. Raw traces live under the attempt
directory only until extraction; the attempt record keeps digests, sizes, usage, verdicts, and the
raw hashes.
"""
import argparse
import json
import os
import shutil
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from tokcmp import checkers, clients, extract, fixture, report, retention  # noqa: E402
from tokcmp.canonical import sha256_hex, text_digest  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))


def load_json(path):
    with open(path) as f:
        return json.load(f)


def abspath_from_repo(repo, p):
    return p if os.path.isabs(p) else os.path.join(repo, p)


def tasks_hash():
    data = open(os.path.join(HERE, "tasks.json"), "rb").read()
    return sha256_hex(data)


def run_attempt(client, task, arm, attempt, cfg, tasks, repo, commit, out_root):
    cell_dir = os.path.join(out_root, client, task, arm)
    attempt_dir = os.path.join(cell_dir, "a%d" % attempt)
    record_path = os.path.join(attempt_dir, "attempt.json")
    if os.path.exists(record_path):
        return load_json(record_path), False
    if os.path.exists(attempt_dir):
        shutil.rmtree(attempt_dir)
    os.makedirs(attempt_dir)
    raw_dir = os.path.join(attempt_dir, "raw")
    os.makedirs(raw_dir)
    fixture_dir = os.path.join(attempt_dir, "fixture")
    fixture.build(repo, commit, fixture_dir)
    relay_dir = os.path.join(attempt_dir, "relay")
    nonce = clients.new_nonce()
    prompt = tasks["tasks"][task]
    if client == "claude":
        cmd = clients.claude_command(arm, prompt, attempt_dir, cfg, nonce, relay_dir, fixture_dir)
    else:
        cmd = clients.codex_command(arm, prompt, attempt_dir, cfg, nonce, relay_dir, fixture_dir)
    stream = os.path.join(raw_dir, "stream.jsonl")
    stderr = os.path.join(raw_dir, "stderr.txt")
    if os.environ.get("TOKCMP_KEEP_RAW"):
        with open(os.path.join(raw_dir, "command.json"), "w") as f:
            json.dump({"cmd": cmd, "cwd": fixture_dir}, f, indent=1)
    run = clients.run_command(cmd, fixture_dir, stream, stderr, cfg["timeout_s"])
    parsed = extract.parse_claude(stream) if client == "claude" else extract.parse_codex(stream)
    relay = extract.parse_relay(relay_dir)
    correlation = extract.correlate(parsed["mcp_calls"], relay)
    comp_status, comp_reasons = extract.compliance(client, arm, parsed, relay)
    attribution = extract.attribution(arm, parsed, correlation)
    answer = parsed.get("answer")
    correctness = checkers.check(task, answer, fixture_dir)
    completed = run["exit_code"] == 0 and not run["timed_out"] and not parsed.get("is_error")
    if client == "codex" and parsed.get("errors"):
        completed = False
    usage = parsed.get("usage")
    tool_calls = []
    relay_reqs = list(relay["requests"])
    for c in parsed["mcp_calls"]:
        entry = {k: c.get(k) for k in ("server", "tool", "args_sha256", "args_bytes", "result_sha256",
                                        "client_recorded_result_bytes", "is_error")}
        match = next((r for r in relay_reqs if r["tool"] == c.get("tool") and r["args_sha256"] == c.get("args_sha256")), None)
        if match is not None:
            relay_reqs.remove(match)
            resp = match.get("response") or {}
            entry["relay_result_text_bytes"] = resp.get("relay_result_text_bytes")
            entry["relay_response_frame_bytes"] = resp.get("relay_response_frame_bytes")
            entry["relay_error_code"] = resp.get("error_code")
            entry["relay_is_error"] = resp.get("is_error")
        tool_calls.append(entry)
    answer_sha, answer_bytes = text_digest(answer) if isinstance(answer, str) else (None, None)
    failure_excerpt = None if completed else retention.scrubbed_tail(stderr)
    if os.environ.get("TOKCMP_KEEP_RAW"):
        # Debugging aid only: raw traces stay in place and the record says so. Never for runs
        # that are retained as evidence.
        raw_hashes = {k: {"sha256": v[0], "bytes": v[1]} for k, v in
                      ((os.path.relpath(os.path.join(d, f), raw_dir), retention.sha256_file(os.path.join(d, f)))
                       for d, _, fs in os.walk(raw_dir) for f in fs)}
        raw_disposition = "KEPT FOR DEBUGGING (TOKCMP_KEEP_RAW); not evidence"
    else:
        raw_hashes = {k: {"sha256": v[0], "bytes": v[1]} for k, v in retention.retire_raw(raw_dir).items()}
        raw_disposition = "hashed and deleted"
    shutil.rmtree(fixture_dir, ignore_errors=True)
    for name in ("mcp.json", "settings.json"):
        p = os.path.join(attempt_dir, name)
        if os.path.exists(p):
            os.remove(p)
    forbidden = fixture.forbidden_strings(fixture_dir)
    repo_root = os.path.abspath(repo)
    if relay["present"]:
        retention.sanitize_manifest(os.path.join(relay_dir, relay["manifest_file"]),
                                    [(fixture_dir, "<fixture>"), (relay_dir, "<relay>"), (repo_root, "<repo>")])
    record = {
        "schema": "token-compare attempt record v1",
        "client": client, "task": task, "arm": arm, "attempt": attempt, "warmup": attempt == 0,
        "commit": commit, "tasks_sha256": tasks_hash(), "nonce_sha256": text_digest(nonce)[0],
        "started_unix_ms": run["started_unix_ms"], "wall_ms": run["wall_ms"],
        "exit_code": run["exit_code"], "timed_out": run["timed_out"], "completed": completed,
        "client_version": parsed.get("client_version"), "model": parsed.get("model") or (cfg["codex"]["model"] if client == "codex" else None),
        "model_source": "client init line" if parsed.get("model") else "pinned by -c model (Codex reports none)",
        "usage": usage, "total_tokens": extract.total_tokens(usage),
        "usage_formula": ("Claude Code result.usage: uncached_input=input_tokens, cache_write=cache_creation_input_tokens, cache_read=cache_read_input_tokens, output=output_tokens; total=sum"
                          if client == "claude" else
                          "Codex sum of turn.completed.usage: input_tokens (includes cached), cached_input_tokens, cache_write_input_tokens, output_tokens (includes reasoning); total=input+output"),
        "num_turns": parsed.get("num_turns") if client == "claude" else parsed.get("turns"),
        "tool_calls": tool_calls, "native_tool_uses": len(parsed.get("native_tools", [])),
        "native_tool_names": sorted(set(parsed.get("native_tools", []))),
        "compliance": {"status": comp_status, "reasons": comp_reasons},
        "correlation": correlation, "attribution": attribution,
        "relay": {"present": relay["present"], "complete": relay["complete"], "reason": relay["reason"],
                  "frames_in": relay["frames_in"], "frames_out": relay["frames_out"],
                  "tools_call_requests": len(relay["requests"])},
        "answer_sha256": answer_sha, "answer_bytes": answer_bytes,
        "correctness": correctness, "checker_version": checkers.CHECKER_VERSION,
        "raw_traces": raw_hashes, "raw_disposition": raw_disposition,
        "failure_stderr_excerpt": failure_excerpt,
    }
    text = json.dumps(record, sort_keys=True)
    hits = retention.scan_bytes(text.encode("utf-8"), forbidden)
    for p in os.listdir(attempt_dir):
        full = os.path.join(attempt_dir, p)
        if os.path.isfile(full):
            hits += ["%s: %s" % (p, h) for h in retention.scan_file(full, forbidden)]
        elif os.path.isdir(full):
            for f in os.listdir(full):
                hits += ["%s/%s: %s" % (p, f, h) for h in retention.scan_file(os.path.join(full, f), forbidden)]
    record["retention_scan"] = {"forbidden_strings": len(forbidden), "hits": hits}
    with open(record_path, "w") as f:
        f.write(json.dumps(record, sort_keys=True, indent=1) + "\n")
    return record, True


def cmd_run(args):
    repo = os.path.abspath(args.repo)
    cfg = load_json(args.config)
    for k in ("server_bin", "relay_bin", "hook_extract"):
        cfg[k] = abspath_from_repo(repo, cfg[k])
    tasks = load_json(os.path.join(HERE, "tasks.json"))
    task_ids = args.tasks.split(",") if args.tasks else list(tasks["tasks"].keys())
    arms = args.arms.split(",") if args.arms else None
    attempts = args.attempts or tasks["attempts_per_cell"]
    clients_ = [args.client] if args.client else ["claude", "codex"]
    args.out = os.path.abspath(args.out)
    os.makedirs(args.out, exist_ok=True)
    started = time.time()
    done = 0
    for client in clients_:
        for task in task_ids:
            order = arms or list(tasks["arm_order_by_task"][task])
            for arm in order:
                for attempt in range(attempts):
                    rec, fresh = run_attempt(client, task, arm, attempt, cfg, tasks, repo, args.commit, args.out)
                    done += 1
                    flag = "ran" if fresh else "kept"
                    print("%s %s/%s/%s a%d %s exit=%s completed=%s compliant=%s attribution=%s correct=%s total=%s wall=%ss" % (
                        flag, client, task, arm, attempt, "timeout" if rec["timed_out"] else "", rec["exit_code"],
                        rec["completed"], rec["compliance"]["status"], rec["attribution"],
                        rec["correctness"]["verdict"], rec["total_tokens"], rec["wall_ms"] // 1000), flush=True)
    print("done %d attempts in %ds" % (done, int(time.time() - started)))


PROOF_PROMPT = (
    "Use the token-saver MCP tools and nothing else, in this order, then report what each returned "
    "in one line each: (1) workspace_outline with no arguments; (2) text_search for the literal "
    "string MAX_OUTBOUND_FRAME_BYTES; (3) file_slice of src/lib.rs with start_line 20 and max_lines "
    "10; (4) file_slice of the path does/not/exist.rs, which should fail, and quote its error text "
    "exactly; (5) workspace_outline with depth 4, which should fail, and quote its error text exactly."
)


def cmd_proof(args):
    """One attempt per client that calls every workspace tool, including two erroneous calls, and
    records the same sanitized attempt record as the comparison (task PROOF, arm B)."""
    repo = os.path.abspath(args.repo)
    cfg = load_json(args.config)
    for k in ("server_bin", "relay_bin", "hook_extract"):
        cfg[k] = abspath_from_repo(repo, cfg[k])
    tasks = load_json(os.path.join(HERE, "tasks.json"))
    tasks["tasks"]["PROOF"] = PROOF_PROMPT
    args.out = os.path.abspath(args.out)
    os.makedirs(args.out, exist_ok=True)
    for client in ([args.client] if args.client else ["claude", "codex"]):
        rec, _ = run_attempt(client, "PROOF", "B", 0, cfg, tasks, repo, args.commit, args.out)
        tools = sorted({c["tool"] for c in rec["tool_calls"]})
        # Codex's item result omits `isError`; the relay's record of the response is authoritative.
        errors = [c for c in rec["tool_calls"] if c.get("is_error") or c.get("relay_is_error")]
        rec["proof"] = {
            "tools_called": tools,
            "all_three_tools": tools == ["file_slice", "text_search", "workspace_outline"],
            "tool_error_calls": len(errors),
            "every_call_correlated": rec["correlation"]["status"] == "complete",
            "attribution": rec["attribution"],
        }
        path = os.path.join(args.out, client, "PROOF", "B", "a0", "attempt.json")
        with open(path, "w") as f:
            f.write(json.dumps(rec, sort_keys=True, indent=1) + "\n")
        print("%s proof: tools=%s errors=%d correlation=%s attribution=%s completed=%s" % (
            client, tools, len(errors), rec["correlation"]["status"], rec["attribution"], rec["completed"]))


def cmd_report(args):
    records = []
    for dirpath, _, files in os.walk(args.runs):
        if "attempt.json" in files:
            records.append(load_json(os.path.join(dirpath, "attempt.json")))
    rep = report.build_report(records)
    rep["records"] = len(records)
    rep["tasks_sha256"] = tasks_hash()
    rep["notes"] = args.note or []
    rep["checker_version"] = checkers.CHECKER_VERSION
    os.makedirs(args.out, exist_ok=True)
    with open(os.path.join(args.out, "report.json"), "w") as f:
        f.write(report.dumps(rep) + "\n")
    with open(os.path.join(args.out, "report.md"), "w") as f:
        f.write(report.render_markdown(rep))
    if args.copy_records:
        rec_dir = os.path.join(args.out, "attempts")
        os.makedirs(rec_dir, exist_ok=True)
        for r in records:
            name = "%s-%s-%s-a%d.json" % (r["client"], r["task"], r["arm"], r["attempt"])
            with open(os.path.join(rec_dir, name), "w") as f:
                f.write(json.dumps(r, sort_keys=True, indent=1) + "\n")
    print("report written: %d records, %d cells, %d comparisons" % (len(records), len(rep["cells"]), len(rep["comparisons"])))


def main():
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--repo", required=True)
    r.add_argument("--commit", required=True)
    r.add_argument("--out", required=True)
    r.add_argument("--client")
    r.add_argument("--tasks")
    r.add_argument("--arms")
    r.add_argument("--attempts", type=int)
    r.add_argument("--config", default=os.path.join(HERE, "compare.json"))
    r.set_defaults(fn=cmd_run)
    pr = sub.add_parser("proof")
    pr.add_argument("--repo", required=True)
    pr.add_argument("--commit", required=True)
    pr.add_argument("--out", required=True)
    pr.add_argument("--client")
    pr.add_argument("--config", default=os.path.join(HERE, "compare.json"))
    pr.set_defaults(fn=cmd_proof)
    q = sub.add_parser("report")
    q.add_argument("--runs", required=True)
    q.add_argument("--out", required=True)
    q.add_argument("--copy-records", action="store_true")
    q.add_argument("--note", action="append", help="recorded verbatim in the report (regrading, reruns)")
    q.set_defaults(fn=cmd_report)
    args = p.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
