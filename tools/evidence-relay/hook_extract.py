#!/usr/bin/env python3 -I
"""Claude Code PostToolUse hook extractor: persists allowlisted fields only.
usage: hook_extract.py <out.jsonl> <expected_nonce>

Records the hook event name, tool name, a SHA-256 prefix of the tool-use id, the duration, the
nonce match for connection_probe, and digests of the tool input (canonical JSON) and of the tool
response text. No argument value, path, name, or content is persisted.
"""
import hashlib, json, sys, time

out_path, expected = sys.argv[1], sys.argv[2]
payload = json.load(sys.stdin)


def canonical_string(s):
    out = ['"']
    for ch in s:
        o = ord(ch)
        if ch == '"':
            out.append('\\"')
        elif ch == '\\':
            out.append('\\\\')
        elif ch == '\b':
            out.append('\\b')
        elif ch == '\f':
            out.append('\\f')
        elif ch == '\n':
            out.append('\\n')
        elif ch == '\r':
            out.append('\\r')
        elif ch == '\t':
            out.append('\\t')
        elif o < 0x20:
            out.append('\\u%04x' % o)
        else:
            out.append(ch)
    out.append('"')
    return ''.join(out)


def canonical(v):
    """Canonical JSON; raises ValueError for a non-integer number."""
    if v is True:
        return 'true'
    if v is False:
        return 'false'
    if v is None:
        return 'null'
    if isinstance(v, int):
        return str(v)
    if isinstance(v, float):
        raise ValueError('non-integer number')
    if isinstance(v, str):
        return canonical_string(v)
    if isinstance(v, list):
        return '[' + ','.join(canonical(x) for x in v) + ']'
    if isinstance(v, dict):
        items = sorted(v.items(), key=lambda kv: [ord(c) for c in kv[0]])
        return '{' + ','.join(canonical_string(k) + ':' + canonical(x) for k, x in items) + '}'
    raise TypeError(type(v))


def sha16(data):
    return hashlib.sha256(data).hexdigest()[:16]


tool_input = payload.get("tool_input")
nonce = tool_input.get("nonce") if isinstance(tool_input, dict) else None
rec = {
    "unix_ms": int(time.time() * 1000),
    "hook_event_name": payload.get("hook_event_name"),
    "tool_name": payload.get("tool_name"),
    "tool_use_id_sha256": sha16(str(payload.get("tool_use_id", "")).encode()),
    "duration_ms": payload.get("duration_ms"),
    "nonce_match": nonce == expected,
}
if isinstance(tool_input, (dict, list)):
    try:
        c = canonical(tool_input).encode("utf-8")
        rec["tool_input_sha256"] = sha16(c)
        rec["tool_input_bytes"] = len(c)
    except ValueError:
        rec["tool_input_sha256"] = None
        rec["tool_input_canonical"] = False
resp = payload.get("tool_response")
if isinstance(resp, (list, dict)):
    text = json.dumps(resp)
    rec["response_mentions_expected_nonce"] = expected in text
    rec["response_bytes"] = len(text)
    # The text block, as Claude Code reports it: a list of content blocks or a dict holding one.
    blocks = resp if isinstance(resp, list) else resp.get("content") if isinstance(resp.get("content"), list) else None
    if blocks:
        first = blocks[0] if isinstance(blocks[0], dict) else {}
        t = first.get("text")
        if isinstance(t, str):
            rec["tool_response_sha256"] = sha16(t.encode("utf-8"))
            rec["tool_response_text_bytes"] = len(t.encode("utf-8"))
    if isinstance(resp, dict) and isinstance(resp.get("isError"), bool):
        rec["is_error"] = resp["isError"]
with open(out_path, "a") as f:
    f.write(json.dumps(rec) + "\n")
