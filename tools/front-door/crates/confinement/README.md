# Confinement

provenant-confinement owns permission policy, disclosure/redaction, verified Git
evidence, output custody, instruction quarantine, and registered worktree management. Filesystem
and process effects use supervisor; callers use supervisor locks/publication
directly rather than a second confinement filesystem API.

Permission compilation consumes typed provider specifications and enforcement
observations constructed by the providers owner. Writer authority binds a held
linked `VerifiedWorktree`, `GitRefName` and the shared Git directory boundaries;
records retain their digest.
`WorktreeWriteAuthority::for_worktree` also takes the host home to derive the
single `Library/Caches/provenant-lane-cargo/<sha256>` cache. Codex writer
preparation sets `CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` to its
`target/` and uses a lane-private `CARGO_HOME`. Writer lanes download crates
into that private registry. Read-only lanes build offline, with their
`CARGO_HOME` registry and Git directories linked read-only to the host's
stores. Lanes should use these values without overriding them. For lint-only runs, both build
directories may use `PROVENANT_LANE_CARGO_LINT_TARGET`. The cache hash binds
canonical worktree and common Git paths plus held directory identities, so
same-named and recreated worktrees do not share a lane cache. Host builds use
separate target directories outside this namespace.
Before collecting `required_anchors`, call `prepare_writer_directories` under
writer custody; pass the same independently resolved home in `Anchors.home`.
This prepares the exact cache and other derived directories. Each writer uses a
complete Git directory under attempt scratch, including its index, HEAD, refs,
merge and rebase state. Its private object store reads the shared store through
`objects/info/alternates`. Stash, rebase, merge and private garbage collection
need no individual file grants. The linked admin directory and shared Git
directory stay read-only throughout their subtrees, including refs, config,
hooks and info. The host imports through Git receive quarantine before publishing
the real branch. Existing cache and lane-parent directories
must belong to the effective user with mode 0700; setup refuses mismatches
without changing their metadata. Compilation rechecks the held cache.
Shared config, tracking refs and packed refs stay host-owned. Hard links
are denied; symlinks are denied only in provider and private state. Toolchain, network and write-root
policy is in the [supervisor spec](../../../../docs/specs/supervisor.md#permission-policy).

No provider profile files are loaded here. Unsupported or nested enforcement refuses before launch. Linux builds
and tests policy/evidence/worktree code but has no enforcing backend.

Safe Git requires a pinned executable supporting `--attr-source`; it disables
tracked attributes with the format-specific empty tree and refuses configured
filters, includes, and repository `info/attributes`. Reads never create objects.
Git probes run once when opening a verified repository. Status and diffs ignore
submodules, and gitlinks require review.

Instruction snapshots bind a private quarantine directory on the repository's
device **before launch**. Restoration moves the observed inode there with atomic
no-replace semantics, verifies it, then publishes the baseline only into a vacant
name. Concurrent open-writer bytes remain on the retained inode. Cross-device
archives refuse prelaunch; there is no copy/unlink fallback. Run still owns the
terminal/quarantine attestations and settlement decision.

CLI owns command bindings and cleanup routing.
Project identity is a persisted random host-local config binding, shared by
linked worktrees of the canonical Git common directory.

`ConfinedPlan::new(PlanRequest)` freezes work scope, permission roots, access
mode, Codex clean/inherited config policy and hooks/MCP/plugin policy. Read-only
plans require clean config and deny extensions; inherited config is refused.
This is validated policy, not backend readiness: compile against held anchors
immediately before handoff. Adapters are enabled by default; recordings do not gate launch.

An optional Agy recipe selects one state directory and an attempt-private,
retained `private_state` root, separate from disposable scratch. The OS crate
creates a 0700 home with a private `.gemini/antigravity-cli/bin`, seeds only six
named state files for copy-back, with five eligible for seeding, within a 64 MiB total bound, and never mirrors the user's
home, credentials, config, plugins or executable helpers. No Agy credential
source is added. The owner must supply canonical existing selected directories and call
copy-back only after confirmed provider quiescence.

Copy-back publishes absent files with atomic no-replace semantics. Identical
existing files need no write; changed existing files return a typed conflict
and keep both copies. This deliberately refuses even an unchanged baseline's
replacement because Directory offers no atomic compare-and-replace against an
uncooperative user writer. Run retains the private home on every failure and
owns settlement; this module never removes it. Existing SQLite state requires
a snapshot mechanism and is refused at preparation. Private WAL, SHM or journal
state refuses copy-back rather than silently dropping committed context.

Merged pruning takes explicit branches and retains candidates whose tip is on
the integration first-parent chain, including fast-forward merges. Linked trees
with gitlinks require review. CLI translates its history scope into explicit
candidates.

Read-only plans resolve Git reads through `PlanRequest::with_read_only_repo`
using a host `ProjectBinding`; the common directory is never sourced from a
lane's `.git` marker. Its subtree is readable and explicitly immutable.
`ReadOnlyPolicy` loads the optional host-state project `read-only.json` and
resolves `readOnlyWritable` (default empty) to existing repository-relative
directories without following symlinks. Git and provider config components
are refused, including config nested inside an allowed subtree. The compiler
rechecks component traversal, held roots and hard-link safety before handoff.
