# ADR 0007 — Defer universal retention classes and typed deletion

**Status:** Accepted 2026-07-13; amended 2026-07-15 and 2026-10-26

## Context

Current retention is deliberately report/archive-only. Universal retention
classes and typed deletion would add state fields and lifecycle machinery
before a concrete deletion requirement exists.

## Decision

Use project and risk policy plus bounded run-artifact retention as the current
owners. Do not require a universal retention-class field or class-tag all new
state.

Defer universal retention classes and typed deletion until a concrete deletion
requirement establishes the necessary protections, evidence and refusal
behaviour. Durable run artefacts remain retained until then.

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

Two holders permit lock-named payload disposal: the terminal, unreserved
reconcile retry inside the attempt's `tmp/`, including rustc incremental
`*.lock` files, and cache GC (#1591) inside a verified detached cache tree.
`PayloadTree::claim` in `supervisor::fs` is the only entry point (#1597). It
refuses a root whose identity differs from the holder's observation, or that
is not owned by the effective user or grants group or other access. Reconcile
widens narrower owner bits, such as 0500, before removal. Ordinary directory
handles have no payload removal. Settlement keeps ordinary lock-name
protection while reservations may remain.
Stable owner locks outside `tmp/` remain protected: unlinking a lock and
recreating its name could split writer exclusion across different inodes.

## Consequences

- Existing project-specific retention rules remain authoritative.
- The harness adds no universal class vocabulary, tagging framework or deletion
  contract now.
- A future deletion proposal requires a separately approved scope.
