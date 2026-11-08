"""Extraction of one attempt from the client's raw stream, the hook extract, and the relay
records into a sanitized attempt record: usage, tool calls as digests and sizes, compliance,
correlation, versions. No argument value, path, name, or content is kept."""
import json
import os

from .canonical import args_digest, text_digest
from .clients import TOKEN_SAVER_TOOLS

SERVER_NAME = "token-saver"
CLAUDE_TOOL_PREFIX = "mcp__token-saver__"


def _lines(path):
    if not os.path.exists(path):
        return []
    out = []
    with open(path, "rb") as f:
        for raw in f:
            raw = raw.strip()
            if not raw:
                continue
            try:
                out.append(json.loads(raw.decode("utf-8", "replace")))
            except ValueError:
                continue
    return out


def _result_text(content):
    """Text of a tool result as the client recorded it: a string, or the first text block."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        for b in content:
            if isinstance(b, dict) and b.get("type") == "text" and isinstance(b.get("text"), str):
                return b["text"]
        return None
    if isinstance(content, dict):
        return _result_text(content.get("content"))
    return None


def parse_claude(stream_path):
    """Claude Code `stream-json`: init line, tool_use/tool_result pairs, final result and usage."""
    events = _lines(stream_path)
    rec = {"client": "claude", "client_version": None, "model": None, "tool_uses": [],
           "tool_results": {}, "answer": None, "usage": None, "is_error": None,
           "num_turns": None, "permission_denials": None, "stop_reason": None}
    for o in events:
        t = o.get("type")
        if t == "system" and o.get("subtype") == "init":
            rec["client_version"] = o.get("claude_code_version") or o.get("version")
            rec["model"] = o.get("model")
        elif t == "assistant":
            for b in (o.get("message") or {}).get("content", []) or []:
                if isinstance(b, dict) and b.get("type") == "tool_use":
                    rec["tool_uses"].append({"id": b.get("id"), "name": b.get("name"),
                                             "input": b.get("input")})
        elif t == "user":
            for b in (o.get("message") or {}).get("content", []) or []:
                if isinstance(b, dict) and b.get("type") == "tool_result":
                    rec["tool_results"][b.get("tool_use_id")] = {
                        "text": _result_text(b.get("content")),
                        "is_error": bool(b.get("is_error", False))}
        elif t == "result":
            rec["answer"] = o.get("result") if isinstance(o.get("result"), str) else None
            u = o.get("usage") or {}
            if u:
                rec["usage"] = {
                    "uncached_input": u.get("input_tokens"),
                    "cache_write": u.get("cache_creation_input_tokens"),
                    "cache_read": u.get("cache_read_input_tokens"),
                    "output": u.get("output_tokens"),
                }
            rec["is_error"] = o.get("is_error")
            rec["num_turns"] = o.get("num_turns")
            rec["permission_denials"] = len(o.get("permission_denials") or [])
            rec["stop_reason"] = o.get("stop_reason")
    calls = []
    native = []
    for tu in rec["tool_uses"]:
        name = tu["name"] or ""
        if name.startswith("mcp__"):
            server = name.split("__")[1] if name.count("__") >= 2 else None
            tool = name[len("mcp__%s__" % server):] if server else name
            result = rec["tool_results"].get(tu["id"], {})
            a_sha, a_bytes = args_digest(tu["input"])
            r_sha, r_bytes = (text_digest(result["text"]) if result.get("text") is not None else (None, None))
            calls.append({"source": "client", "server": server, "tool": tool,
                          "args_sha256": a_sha, "args_bytes": a_bytes,
                          "result_sha256": r_sha, "client_recorded_result_bytes": r_bytes,
                          "is_error": result.get("is_error")})
        else:
            native.append(name)
    rec["mcp_calls"] = calls
    rec["native_tools"] = native
    return rec


def parse_codex(stream_path):
    """Codex `exec --json`: items and turn usage summed over the run."""
    events = _lines(stream_path)
    rec = {"client": "codex", "client_version": None, "model": None, "mcp_calls": [],
           "native_tools": [], "answer": None, "usage": None, "turns": 0, "errors": []}
    usage = {"input": 0, "cached": 0, "cache_write": 0, "output": 0, "reasoning_output": 0}
    saw_usage = False
    for o in events:
        t = o.get("type")
        if t == "item.completed":
            it = o.get("item") or {}
            kind = it.get("type")
            if kind == "mcp_tool_call":
                res = it.get("result")
                text = _result_text(res) if isinstance(res, (dict, list)) else None
                is_error = None
                if isinstance(res, dict):
                    is_error = bool(res.get("isError", False))
                a_sha, a_bytes = args_digest(it.get("arguments"))
                r_sha, r_bytes = text_digest(text) if text is not None else (None, None)
                rec["mcp_calls"].append({"source": "client", "server": it.get("server"),
                                         "tool": it.get("tool"), "args_sha256": a_sha,
                                         "args_bytes": a_bytes, "result_sha256": r_sha,
                                         "client_recorded_result_bytes": r_bytes,
                                         "is_error": is_error,
                                         "protocol_error": it.get("error") is not None,
                                         "status": it.get("status")})
            elif kind == "agent_message":
                if isinstance(it.get("text"), str):
                    rec["answer"] = it["text"]
            elif kind in ("reasoning",):
                pass
            else:
                rec["native_tools"].append(kind or "unknown")
        elif t == "turn.completed":
            u = o.get("usage") or {}
            saw_usage = True
            rec["turns"] += 1
            usage["input"] += u.get("input_tokens", 0) or 0
            usage["cached"] += u.get("cached_input_tokens", 0) or 0
            usage["cache_write"] += u.get("cache_write_input_tokens", 0) or 0
            usage["output"] += u.get("output_tokens", 0) or 0
            usage["reasoning_output"] += u.get("reasoning_output_tokens", 0) or 0
        elif t in ("error", "turn.failed"):
            rec["errors"].append(str(o.get("message") or o.get("error") or t)[:200])
    if saw_usage:
        rec["usage"] = {
            "uncached_input": usage["input"] - usage["cached"],
            "cache_write": usage["cache_write"],
            "cache_read": usage["cached"],
            "output": usage["output"],
            "reasoning_output": usage["reasoning_output"],
            "codex_input_total": usage["input"],
        }
    return rec


def total_tokens(usage):
    """Complete, unweighted token measure: every category once. Codex `input` already contains
    its cached part, so the total is input plus output; Claude Code's categories are disjoint."""
    if usage is None:
        return None
    if "codex_input_total" in usage:
        return usage["codex_input_total"] + usage["output"]
    return (usage["uncached_input"] or 0) + (usage["cache_write"] or 0) + (usage["cache_read"] or 0) + (usage["output"] or 0)


def parse_relay(relay_dir):
    """Relay records as ordered request/response pairs for tools/call, plus manifest facts."""
    out = {"present": False, "complete": None, "reason": None, "requests": [], "frames_in": None,
           "frames_out": None, "any_traffic": False}
    if not relay_dir or not os.path.isdir(relay_dir):
        return out
    manifests = sorted(p for p in os.listdir(relay_dir) if p.endswith(".manifest.json"))
    if not manifests:
        return out
    out["present"] = True
    m = json.load(open(os.path.join(relay_dir, manifests[-1])))
    out["complete"] = m.get("complete")
    out["reason"] = m.get("reason")
    out["frames_in"] = m.get("frames_in")
    out["frames_out"] = m.get("frames_out")
    out["manifest_file"] = manifests[-1]
    records_file = m.get("records_file")
    recs = _lines(os.path.join(relay_dir, records_file)) if records_file else []
    out["any_traffic"] = bool(recs)
    by_id = {}
    for r in recs:
        if r.get("dir") == "in" and r.get("method") == "tools/call":
            req = {"tool": r.get("tool"), "args_sha256": r.get("args_sha256"),
                   "args_bytes": r.get("args_bytes"), "id_sha256": r.get("id_sha256"),
                   "response": None}
            out["requests"].append(req)
            by_id.setdefault(r.get("id_sha256"), []).append(req)
    for r in recs:
        if r.get("dir") == "out" and r.get("kind") in ("response", "error"):
            pending = by_id.get(r.get("id_sha256"))
            if not pending:
                continue
            req = next((q for q in pending if q["response"] is None), None)
            if req is None:
                continue
            if r.get("kind") == "error":
                req["response"] = {"error_code": r.get("error_code"), "result_sha256": None}
            else:
                req["response"] = {"result_sha256": r.get("result_sha256"),
                                   "relay_result_text_bytes": r.get("result_bytes"),
                                   "relay_response_frame_bytes": r.get("bytes"),
                                   "is_error": r.get("is_error")}
    return out


def correlate(client_calls, relay):
    """Matches the k-th client call with key (tool, args_sha256) to the k-th relay request with
    the same key and compares response digests. Returns a status dict; never claims
    corroboration when counts or digests disagree."""
    if not relay["present"]:
        return {"status": "incomplete", "reason": "no relay capture", "matched": 0}
    if relay["complete"] is not True:
        return {"status": "incomplete", "reason": "relay manifest incomplete: %s" % relay["reason"], "matched": 0}
    def key(c):
        return (c.get("tool"), c.get("args_sha256"))
    groups_client = {}
    for c in client_calls:
        groups_client.setdefault(key(c), []).append(c)
    groups_relay = {}
    for r in relay["requests"]:
        groups_relay.setdefault(key(r), []).append(r)
    if len(client_calls) != len(relay["requests"]):
        return {"status": "incomplete", "reason": "call count differs: client %d, relay %d" % (len(client_calls), len(relay["requests"])), "matched": 0}
    matched = 0
    repeated = 0
    for k, cs in groups_client.items():
        rs = groups_relay.get(k, [])
        if len(rs) != len(cs):
            return {"status": "incomplete", "reason": "per-key count differs for %s" % k[0], "matched": matched}
        if len(cs) > 1:
            repeated += len(cs)
        for c, r in zip(cs, rs):
            resp = r.get("response")
            if resp is None:
                return {"status": "incomplete", "reason": "relay request without response", "matched": matched}
            if "error_code" in resp:
                if c.get("result_sha256") is not None and not (c.get("is_error") or c.get("protocol_error")):
                    return {"status": "incomplete", "reason": "relay error but client recorded a result", "matched": matched}
            else:
                if c.get("result_sha256") != resp.get("result_sha256"):
                    return {"status": "incomplete", "reason": "result digest differs", "matched": matched}
            matched += 1
    for k in groups_relay:
        if k not in groups_client:
            return {"status": "incomplete", "reason": "relay request without client counterpart", "matched": matched}
    note = "corroborated by count and digest, not individually distinguishable" if repeated else None
    return {"status": "complete", "reason": None, "matched": matched, "repeated_calls": repeated, "note": note}


def compliance(client, arm, parsed, relay):
    """Arm rules of plan section 8. Returns (status, reasons)."""
    reasons = []
    mcp = parsed["mcp_calls"]
    if arm == "A":
        if relay["any_traffic"]:
            reasons.append("relay traffic in arm A")
        if mcp:
            reasons.append("MCP tool call in arm A")
    elif arm == "B":
        if client == "claude":
            bad = [n for n in parsed["native_tools"] if n != "ToolSearch"]
            if bad:
                reasons.append("native tool_use in arm B: %s" % sorted(set(bad)))
        else:
            if parsed["native_tools"]:
                reasons.append("non-MCP item in arm B: %s" % sorted(set(parsed["native_tools"])))
        for c in mcp:
            if c.get("server") != SERVER_NAME or c.get("tool") not in TOKEN_SAVER_TOOLS:
                reasons.append("call to another server or tool: %s/%s" % (c.get("server"), c.get("tool")))
    # Arm C is unconstrained in tool choice.
    status = "compliant" if not reasons else "noncompliant"
    return status, reasons


def attribution(arm, parsed, correlation):
    """Server-only attribution needs the specified server, the specified tools, and complete
    correlation. Arm A has nothing to attribute."""
    if arm == "A":
        return "not applicable"
    foreign = [c for c in parsed["mcp_calls"] if c.get("server") != SERVER_NAME or c.get("tool") not in TOKEN_SAVER_TOOLS]
    if foreign:
        return "refused (foreign server or tool)"
    if correlation["status"] != "complete":
        return "unverified"
    return "verified"
