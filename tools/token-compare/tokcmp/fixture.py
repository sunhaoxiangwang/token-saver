"""Fixture construction: this repository at a pinned commit, copied fresh per attempt, with the
canary entries added. No CLAUDE.md or AGENTS.md is written."""
import os
import subprocess

CANARY_DIR = "CANARY_DIR_7f3a"
CANARY_CONTENT = "CANARY_CONTENT_9c1e"
CANARY_FILE = "canary.txt"


def build(repo, commit, dest):
    os.makedirs(dest, exist_ok=False)
    archive = subprocess.run(["git", "-C", repo, "archive", "--format=tar", commit],
                             check=True, capture_output=True).stdout
    subprocess.run(["tar", "-x", "-C", dest], input=archive, check=True)
    os.makedirs(os.path.join(dest, CANARY_DIR))
    with open(os.path.join(dest, CANARY_DIR, "README.txt"), "w") as f:
        f.write("canary directory for the token-compare harness\n")
    with open(os.path.join(dest, CANARY_FILE), "w") as f:
        f.write(CANARY_CONTENT + "\n")
    for forbidden in ("CLAUDE.md", "AGENTS.md"):
        assert not os.path.exists(os.path.join(dest, forbidden)), forbidden
    return dest


def forbidden_strings(fixture_path, extra=()):
    """Substrings that must not appear in any retained artifact."""
    home = os.path.expanduser("~")
    items = [CANARY_DIR, CANARY_CONTENT, fixture_path, home]
    items.extend(extra)
    return [s for s in items if s]
