# ADR-0002: workspace root authority as retained-directory authority

Status: accepted at plan level (TS-M1-REVIEW-004, TS-M1-PLAN-001 revision 4), implemented in
Milestone 1.

## Context

Milestone 1 adds three read-only tools that read below one operator-declared directory, the root.
Path-based confinement (canonicalize, then open) is racy: a component replaced by a symlink between
the check and the open redirects the open. Descriptor-relative, no-follow, single-component opens
remove that race but do not confine reads to the root's *current* subtree: a directory descriptor
refers to a directory object, and the object can be renamed out of the root while a job holds it
(reproduced by the reviewer and by this project on macOS 26.6: a directory opened through the root
descriptor, renamed outside, then given a new file, served that file through the retained
descriptor).

## Decision

- `--root <absolute path>` is opened once at startup, before diagnostics start, with
  `O_RDONLY|O_DIRECTORY|O_CLOEXEC|O_NOFOLLOW|O_NONBLOCK|O_NOCTTY`, and the descriptor is held by the
  worker pool for the process lifetime. This is the only use of ambient path authority. The textual
  root never appears in results or diagnostics.
- Every object is classified with `fstatat(AT_SYMLINK_NOFOLLOW)` before any open. Symlinks and
  objects classified as special files are rejected before any open. Every open is
  `openat(dir, name, O_RDONLY|O_CLOEXEC|O_NOFOLLOW|O_NONBLOCK|O_NOCTTY[|O_DIRECTORY])` of a single
  component relative to a held directory descriptor. `fstat` after the open is authoritative: an
  object that became a special file between classification and open is identified and closed
  unread; nothing but regular files and directories is ever read.
- Authority contract (retained-directory authority). A job opens an object only if, at the instant
  of the open, it is a direct child of a directory object the job holds. Descriptors refer to
  objects, not pathnames: a held directory stays held after a rename, including a rename out of the
  root's subtree, and the root follows its descriptor wherever it is renamed. A job holds non-root
  directory objects only within tool-specific windows (`file_slice`: the walk, before any read;
  `workspace_outline` and `text_search`: one directory visit at a time) and only for the job's
  lifetime. Every resolution starts from the root and sees the tree as it is.
- Unreachable by construction: any object named through `..`, an absolute path, or a symlink; any
  object that was never, at the instant of an open in a job's chain, a direct child of a directory
  the job held.
- Documented residuals, not escapes: content moved or hard-linked into the root or into a held
  directory becomes readable; a mount point inside the root is inside; a renamed root is followed; a
  deleted root resolves children to `not found`; a directory held by a running job and renamed out of
  the root during the job stays readable to that job until the job ends (bounded by the job
  lifetime in ADR-0001's supervision terms: detection within one tick of admission plus 7.5 s, then
  termination). The actor who can perform that rename has write permission on the root directory and
  can equally move the same content into the root.
- The identity walk (re-checking `..` chains against the root) is not required for this contract and
  is not implemented (D4, TS-M1-REVIEW-003). Linux `openat2(RESOLVE_BENEATH)` is out of scope: it
  protects one lookup, not retained descriptors, and is unavailable on macOS.

## Consequences

- The dependency `rustix =1.1.6` (`std`, `alloc`, `fs`) provides `openat`, `statat`, `fstat`,
  `fcntl_getfl`, `fcntl_setfl`, and `Dir`; the crate keeps `#![forbid(unsafe_code)]`.
- Tests C1 to C12 (`tests/workspace_confine.rs`) assert the contract, including the move-out case as
  documented behaviour rather than as containment.
- Operators declare project roots only, never a home directory, and ChatGPT roots only with content
  they would upload; the exclusion and denial lists are guards, not a guarantee (D2).
