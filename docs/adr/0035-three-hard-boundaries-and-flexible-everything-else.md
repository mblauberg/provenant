# ADR 0035: Three hard boundaries, flexible execution and publication disclosure

**Status:** Accepted

Changed: 2026-11-20, #3688: output redaction is opt-in.
Changed: 2026-11-22, #3782: dropped the 1.x handoff paragraph. Both hosts run
2.6.x and the owner has no other installs, so the one-time cut-over no longer
justifies compatibility code.

## Context

Guards added for unobserved risks cost more than they saved: tolerant-config,
land-cleanup and advisory-guard work (#2268, #2256, #2284) repeatedly removed
refusals that blocked ordinary work, while the genuine failures were data loss,
credential exposure and unauthorised change. Open-ended classes such as "Git
isolation" and "data integrity" let every sharing path read as a boundary.

## Decision

This is the single definition of the hard boundaries. Each is an outcome, not a
mechanism:

1. **Credentials and secrets** never reach a lane, provider or output that was
   not granted them. Output masking, including GitHub publication and cross-host
   text, is opt-in and defaults off. GitHub access and its read-only
   credential-store exception are governed by [ADR 0040](0040-open-lane-policy-with-explicit-boundaries.md#permitted-exceptions).
2. **Other lanes' work:** no lane moves, rewrites or deletes another lane's
   refs, commits or uncommitted files, and nothing destroys uncommitted work.
3. **Authority:** user gates, merge, release and ruleset changes need the held
   authority; their knobs refuse malformed values rather than recovering to
   permissive defaults.

A boundary refuses only the action that would breach its outcome. Everything
else, including the mechanisms that hold a boundary (sandboxing, private Git
directories, scratch targets), depth limits and model-policy checks, is a
mechanism or an overridable default: it warns and proceeds, with its source
shown. Invalid user configuration warns and keeps the valid settings. Explicit owner-configured stops are distinct from boundary refusals and
name their source. Malformed input, missing capabilities, invalid state and
failed integrity checks are operation errors: they refuse that one operation
with a typed code ([ADR 0034](0034-refusals-carry-stable-typed-codes.md)), not
a boundary. Heuristic scanners never stop briefs or graph patches, or silently
rewrite execution instructions; warnings identify the field and rule without
matched values. Persisted formats are versioned, and each owner defines its
current field shape.

Interfaces and stored shapes change without compatibility shims, older-shape
readers or aliases. Stored state changes by additive SQLite schema steps within
one state-root format major, or by one declared migration that rewrites the
rows the new types cannot read and advances that store's base version. The
migration runs once on a writable open, and a reader of the older base refuses
until it has run; there is no compatibility path afterwards. Any other
non-additive stored change requires the next root format major and a fresh
state root.

Local execution data, provider histories and graph text remain lossless in
host-owned private state. File modes are defence in depth, not isolation from
same-user processes; lanes receive only authorised state. The `redaction` knob
defaults to `off`; `secrets` opts into masking at publication. The owner decided on
20 November 2026 that modern models are aligned enough not to post credentials,
and masking is hard to make reliable and causes more issues than it guards
against. The modes and their destinations are specified in the
[supervisor spec](../specs/supervisor.md#disclosure-and-safe-git-evidence). Raw
and exported representations retain separate accurate lengths and digests. When
an enabled projection fails, it withholds the affected publication without
changing the local execution outcome.

Provider and host restrictions are explicit, sourced and configurable; ordinary
filesystem, tools and network access remain open by default.

## Consequences

A hard hold needs one of the three boundaries. Reviews treat an automatic
refusal outside them as a defect to loosen. Sharing among the owner's lanes,
hosts and providers (caches, warm targets, co-writers in a worktree, relaxed
checks) is intended. See
[Build for change](../specs/rust-workspace.md#build-for-change-refuse-what-can-lose-data) and the
[supervisor spec](../specs/supervisor.md).

Overturn when: data loss, credential exposure or unauthorised change is traced
to a guard this decision loosened.
