"""Retention: temporary raw traces are hashed and deleted; retained artifacts are scanned for the
canaries, the fixture path, the home directory, and e-mail addresses before they are kept."""
import hashlib
import json
import os
import re
import shutil

EMAIL = re.compile(rb"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest(), os.path.getsize(path)


def scan_bytes(data, forbidden):
    hits = []
    for s in forbidden:
        if s.encode("utf-8") in data:
            hits.append("forbidden string present")
    if EMAIL.search(data):
        hits.append("e-mail address present")
    return hits


def scan_file(path, forbidden):
    with open(path, "rb") as f:
        return scan_bytes(f.read(), forbidden)


def sanitize_manifest(path, replacements):
    """Rewrites host paths in the relay manifest's `server` argv to placeholders."""
    m = json.load(open(path))
    argv = m.get("server")
    if isinstance(argv, list):
        new = []
        for a in argv:
            for old, repl in replacements:
                if old and old in a:
                    a = a.replace(old, repl)
            new.append(a)
        m["server"] = new
        m["argv_sanitized"] = True
    with open(path, "w") as f:
        f.write(json.dumps(m, sort_keys=True) + "\n")


def scrubbed_tail(path, max_lines=8, max_chars=600):
    """Last lines of a raw stderr for failure diagnosis: lines with an e-mail address are dropped,
    the home directory is replaced, and the excerpt is truncated."""
    if not os.path.exists(path):
        return None
    home = os.path.expanduser("~")
    try:
        lines = open(path, "rb").read().decode("utf-8", "replace").splitlines()
    except OSError:
        return None
    kept = [l for l in lines if not EMAIL.search(l.encode("utf-8", "replace"))][-max_lines:]
    text = "\n".join(kept).replace(home, "<home>")
    return text[-max_chars:]


def retire_raw(raw_dir):
    """Hashes every file under raw_dir and removes the directory. Returns {relative: (sha, size)}."""
    hashes = {}
    if os.path.isdir(raw_dir):
        for dirpath, _, files in os.walk(raw_dir):
            for f in files:
                p = os.path.join(dirpath, f)
                hashes[os.path.relpath(p, raw_dir)] = sha256_file(p)
        shutil.rmtree(raw_dir, ignore_errors=True)
    return hashes
