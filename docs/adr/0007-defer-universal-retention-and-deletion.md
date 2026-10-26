# ADR 0007: Defer universal retention classes and typed deletion

**Status:** Accepted 2026-07-13; amended 2026-07-15 and 2026-10-26;
provider-state retention deferral superseded by [ADR
0031](0031-retain-provider-history-and-expire-resume-state.md)

## Context

Provider-state retention is now governed by ADR 0031. Universal retention
classes and typed deletion would add state fields and lifecycle machinery
before a concrete requirement for those abstractions exists.

## Decision

Use project and risk policy plus bounded run-artifact retention as the current
owners. Do not require a universal retention-class field or class-tag all new
state.

Defer universal retention classes and typed deletion until a concrete deletion
requirement establishes the necessary protections, evidence and refusal
behaviour. ADR 0031 supplies that requirement for provider state; other durable
run artefacts remain retained.

Attempt-private `tmp/` is disposable. Writer `TMPDIR` lives in `tmp/candidate-N`.
Terminal commit attempts to remove `tmp/`; `reconcile` retries removal for
terminal attempts once no reservation is retained. It uses the existing
no-follow, identity-checked deletion primitives and reports leftover paths and
reasons without failing reconciliation for a removal error. Live and non-terminal
attempts keep their temporary data.
Unreadable worktree reservations block removal. Reconcile checks worktree
reservations first, then holds the attempt's writer lock during deletion to
exclude read-only guardians. It does not hold the worktree writer lock during
deletion.

The original holders permitting lock-named payload disposal were the terminal,
unreserved reconcile retry inside the attempt's `tmp/`, including rustc
incremental `*.lock` files, and cache GC (#1591) inside a verified detached
cache tree.
ADR 0031 adds verified detached provider-state payloads.
`PayloadTree::claim` in `supervisor::fs` is the only entry point (#1597). It
refuses a root whose identity differs from the holder's observation, or that
is not owned by the effective user or grants group or other access. Reconcile
widens narrower owner bits, such as 0500, before removal. Ordinary directory
handles have no payload removal. Temporary settlement cleanup keeps ordinary
lock-name protection while reservations may remain; proven regenerable
provider caches are detached and disposed of separately after terminal commit.
Stable owner locks outside `tmp/` remain protected: unlinking a lock and
recreating its name could split writer exclusion across different inodes.

Scheduled lane cache GC is approved automatic deletion as of 2026-10-26
(#1591). On macOS, installation schedules collection every six hours by
default. User configuration can disable it or change the idle threshold and
optional free-space floor. Collection is scoped to lane build caches, uses
verified detachment and preserves live writers, reservations and unobservable
owners. A free-space floor collects eligible caches oldest-first by last write
after the orphaned, tombstoned and idle pass. ADR 0031 separately approves
provider-state retention. General state deletion remains outside these
approvals. Automatic removal of merged worktrees is tracked separately in #1683.

## Consequences

- Existing project-specific retention rules remain authoritative.
- The harness adds no universal class vocabulary, tagging framework or deletion
  contract now.
- A future deletion proposal requires a separately approved scope.
