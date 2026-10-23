# Confinement

S3 owns permission policy, disclosure/redaction, verified Git evidence, output
custody, instruction quarantine, and registered worktree management. Filesystem
and process effects use supervisor; callers use supervisor locks/publication
directly rather than a second confinement filesystem API.

Permission compilation requires recorded macOS enforcement observations bound
to the complete provider specification, canonical held anchors, private scratch,
and an opaque `VerifiedWorktree` for writes. No real provider profiles are
bundled. Unsupported or nested enforcement refuses before launch. Linux builds
and tests policy/evidence/worktree code but has no enforcing backend.

Safe Git requires a pinned executable supporting `--attr-source`; it disables
tracked attributes with the format-specific empty tree and refuses configured
filters, includes, and repository `info/attributes`. Reads never create objects.
GitHub merge proof additionally requires an explicit common-directory-bound
repository identity; a remote URL or ambient gh default cannot supply authority.

Instruction snapshots bind a private quarantine directory on the repository's
device **before launch**. Restoration moves the observed inode there with atomic
no-replace semantics, verifies it, then publishes the baseline only into a vacant
name. Concurrent open-writer bytes remain on the retained inode. Cross-device
archives refuse prelaunch; there is no copy/unlink fallback. Run still owns the
terminal/quarantine attestations and settlement decision.
