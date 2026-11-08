# token-compare

The Milestone 1 comparison harness (TS-M1-PLAN-001 revision 4, section 8). Standard library
only; run with `python3 -I`.

```
python3 -I tools/token-compare/compare.py run --repo . --commit <pinned sha> --out evidence/m1-runs
python3 -I tools/token-compare/compare.py report --runs evidence/m1-runs --out docs/evidence/m1/compare --copy-records
python3 -I -m unittest discover -s tools/token-compare/tests
```

- `tasks.json` holds the fixed task wording, the arm order per task, and the attempt count; its
  SHA-256 is recorded in every attempt record.
- `compare.json` pins the client binaries, the model identifiers, Codex's reasoning effort, and
  the 180 s timeout. Client versions and the Claude Code model are read back from client output;
  Codex reports no model id in `exec --json`, so its record carries the pinned value and says so.
- Each attempt runs in a fresh fixture copy (this repository at the pinned commit plus the canary
  entries) with a fresh relay directory and nonce. Raw traces are hashed and deleted after
  extraction; the attempt record keeps digests, sizes, usage, verdicts, and raw hashes only, and is
  scanned for the canaries, the fixture path, the home directory, and e-mail addresses.
- The report never aggregates into a percentage or a cost. `total_tokens` is the complete,
  unweighted token measure; every category is shown beside it. A saving is stated only under the
  pre-registered rule: both cells resolved (attempts 1 to 6 exactly once, every one with trusted
  usage; a failed attempt reporting zero tokens is usage unavailable), every arm B attempt compliant
  with verified attribution and at least one token-saver call, arm B correct count at least arm A's,
  arm B total tokens per correct answer lower, and success-conditioned medians agreeing.
- `checker_version` (currently 3) is recorded in every attempt record and report; a changed checker
  rule requires reruns because raw answers are deleted, and the originals are preserved beside the
  reruns.
