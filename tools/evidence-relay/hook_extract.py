#!/usr/bin/env python3 -I
"""Claude Code PostToolUse hook extractor: persists allowlisted fields only.
usage: hook_extract.py <out.jsonl> <expected_nonce>"""
import hashlib, json, sys, time
out_path, expected = sys.argv[1], sys.argv[2]
payload = json.load(sys.stdin)
tool_input = payload.get("tool_input") or {}
nonce = tool_input.get("nonce") if isinstance(tool_input, dict) else None
rec = {
    "unix_ms": int(time.time() * 1000),
    "hook_event_name": payload.get("hook_event_name"),
    "tool_name": payload.get("tool_name"),
    "tool_use_id_sha256": hashlib.sha256(str(payload.get("tool_use_id", "")).encode()).hexdigest()[:16],
    "duration_ms": payload.get("duration_ms"),
    "nonce_match": nonce == expected,
}
resp = payload.get("tool_response")
if isinstance(resp, (list, dict)):
    text = json.dumps(resp)
    rec["response_mentions_expected_nonce"] = expected in text
    rec["response_bytes"] = len(text)
with open(out_path, "a") as f:
    f.write(json.dumps(rec) + "\n")
