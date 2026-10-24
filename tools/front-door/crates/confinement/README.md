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

`ConfinedPlan::new(PlanRequest)` freezes work scope, permission roots, access
mode, Codex clean/inherited config policy and hooks/MCP/plugin policy. Read-only
plans require clean config and deny extensions; inherited config is refused.
This is validated policy, not backend readiness: compile against held anchors
immediately before handoff. Provider activation still gates every launch.

An optional Agy recipe selects one state directory and an attempt-private,
retained `private_state` root, separate from disposable scratch. Supervisor
creates a 0700 home with a private `.gemini/antigravity-cli/bin`, seeds only six
named state files for copy-back, with five eligible for seeding, within a 64 MiB total bound, and never mirrors the user's
home, credentials, config, plugins or executable helpers. No Agy credential
source is added. The owner must supply canonical existing selected directories and call
copy-back only after confirmed provider quiescence.

Copy-back publishes absent files with atomic no-replace semantics. Identical
existing files need no write; changed existing files return a typed conflict
and keep both copies. This deliberately refuses even an unchanged baseline's
replacement because SafeFs offers no atomic compare-and-replace against an
uncooperative user writer. Run retains the private home on every failure and
owns settlement; this module never removes it. Existing SQLite state requires
a snapshot mechanism and is refused at preparation. Private WAL, SHM or journal
state refuses copy-back rather than silently dropping committed context.

The Python `agy_home.py` remains live: `provider_exec.py` imports it and calls
`needs_ps_shim`, `stage_ps_shim`, `stage_agy_home` and `finish_agy_home`.
Its tests remain until that caller and the Python ps shim retire.

Merged pruning takes explicit branches and retains candidates whose tip is on
the integration first-parent chain, including fast-forward merges. Linked trees
with gitlinks require review. The unsafe library `since` path is removed; CLI
step 4 owns translating its frozen history scope into explicit candidates.
