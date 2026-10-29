# Memory and scratch

Keep durable knowledge in the owning project document. Private harness memory
contains only cross-project preferences. Run records own attempts, output and
custody.

Harness scratch (helper scripts, drafts, review copies) stays in the harness
scratch directory; for Claude Code, that tree is set by `CLAUDE_CODE_TMPDIR`.
Scratch is disposable. Anything needed later moves to its owner first: the
project document, the issue or PR body, the run record, the chair log, or the
session checkpoint.

Store temporary analysis under an authorised scratch root. Supervisor guardian
settlement deletes classified scratch only after quiescence and writer
exclusion. Retained history and unclassified files remain. Do not run a broad
cleanup sweep. Use `provenant worktree remove --merged` with a pinned
integration OID and explicit branch/range for authorised merged worktrees.
