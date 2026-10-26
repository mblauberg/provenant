# ADR 0007: Deletion requires custody of disposable state

**Status:** Accepted

Changed: 2026-10-26, #1754.

## Context

Temporary payloads, lane caches and regenerable output need collection without
splitting locks, deleting a live writer's files or treating durable results as
cache. A universal retention-class vocabulary does not establish that custody.

## Decision

Keep durable run artefacts unless a separately approved retention rule applies.
[ADR 0031](0031-retain-provider-history-and-expire-resume-state.md) owns the
provider-state rule. Use the existing no-follow directory operations,
identity-checked detachment and exclusive holder for disposable payloads.
`PayloadTree::claim` in the OS crate is the only payload-removal entry point;
it requires the observed root identity, effective-user ownership and private
permissions. Stable owner locks remain outside disposable trees, because
unlinking and recreating a lock name can split exclusion across inodes.

Approved collection classes are:

- Terminal attempt scratch: preserve live attempts and retained or unreadable
  reservations; reconcile holds the attempt's writer lock during removal.
- Lane build caches: detach only a verified eligible cache tree, preserving
  live writers, reservations and unobservable owners. Scheduled collection is
  authorised by [#1591](https://github.com/mblauberg/provenant/issues/1591).
- Merged worktrees and their own caches: fetch the upstream default branch,
  prove the lane's commits merged, require clean registered trees and known
  terminal lanes, then remove expected-old refs. Upstream tips and its
  first-parent history stay. A failed fetch grants no deletion authority
  ([#1683](https://github.com/mblauberg/provenant/issues/1683)).
- Explicitly declared regenerable output: require a clean registered worktree,
  known terminal lanes, writer exclusion and no writes in the preceding
  24 hours. Protect tracked and configured keep paths; never follow symlinks;
  recheck identity and recursive write times after detachment
  ([#1687](https://github.com/mblauberg/provenant/issues/1687)).
- Verified detached provider-state payloads under ADR 0031.

Only the scratch, verified cache, declared regenerable-output and provider-state
payload holders may dispose of lock-named payload files. Worktree collection
does not gain that bypass. Ordinary directory handles retain lock-name
protection. Unknown or unreadable custody blocks removal; errors preserve
evidence and permit a later retry.

## Consequences

No universal class tagging or general state-deletion framework is introduced.
Bare GC collects eligible caches and previews worktree/output cleanup; those
classes require `--apply`. `--dry-run` previews every class. The scheduled
collector passes `--apply`; configuration controls its schedule and eligibility.
Deletion implementation belongs to OS, supervisor, confinement and run owners,
not to a new retention service.
