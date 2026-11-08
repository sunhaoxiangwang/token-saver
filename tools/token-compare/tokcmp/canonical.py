"""Canonical JSON and digests shared with the evidence relay and the hook extractor.

Objects: keys sorted by code point, no whitespace, integers in decimal, true/false/null, strings
escaping only `"`, `\\` and control characters (short forms for U+0008, U+0009, U+000A, U+000C,
U+000D; `\\u00xx` lowercase otherwise), everything else as raw UTF-8. A non-integer number is not
canonical and raises ValueError.
"""
import hashlib


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


def sha256_hex(data):
    return hashlib.sha256(data).hexdigest()


def args_digest(arguments):
    """(args_sha256, args_bytes) or (None, None) when the arguments are not canonical."""
    try:
        c = canonical(arguments if arguments is not None else {}).encode('utf-8')
    except ValueError:
        return None, None
    return sha16(c), len(c)


def text_digest(text):
    data = text.encode('utf-8')
    return sha16(data), len(data)
