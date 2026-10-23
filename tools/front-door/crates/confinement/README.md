# Confinement

S3 owns permission policy, disclosure/redaction, verified Git evidence, output
custody, instruction quarantine, and registered worktree management. Filesystem
and process effects use supervisor; callers use supervisor locks/publication
directly rather than a second confinement filesystem API.

Permission compilation consumes typed provider specifications and enforcement
observations constructed by the providers owner. Git write grants derive from a
held linked `VerifiedWorktree` and `GitRefName`; records retain their digest.
No provider profile files are loaded here. Unsupported or nested enforcement refuses before launch. Linux builds
and tests policy/evidence/worktree code but has no enforcing backend.

Safe Git requires a pinned executable supporting `--attr-source`; it disables
tracked attributes with the format-specific empty tree and refuses configured
filters, includes, and repository `info/attributes`. Reads never create objects.
Git probes run once when opening a verified repository. Status and diffs ignore
submodules, and gitlinks require review. Squash-only pruning is retained until
the binding owner supplies repository-bound GitHub evidence.

Instruction snapshots bind a private quarantine directory on the repository's
device **before launch**. Restoration moves the observed inode there with atomic
no-replace semantics, verifies it, then publishes the baseline only into a vacant
name. Concurrent open-writer bytes remain on the retained inode. Cross-device
archives refuse prelaunch; there is no copy/unlink fallback. Run still owns the
terminal/quarantine attestations and settlement decision.

S3 exports a library. CLI step 4 owns command bindings and cleanup routing.
Project identity is a persisted random host-local config binding, shared by
linked worktrees of the canonical Git common directory.

Merged pruning takes explicit branches and retains candidates whose tip is on
the integration first-parent chain, including fast-forward merges. Linked trees
with gitlinks require review. The unsafe library `since` path is removed; CLI
step 4 owns translating its frozen history scope into explicit candidates.
