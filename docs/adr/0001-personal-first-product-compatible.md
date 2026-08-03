# ADR 0001 — Personal-first, product-compatible posture

**Status:** Accepted 2026-07-13 (human, scoping round 1)

## Context

The 2026-07-13 comprehensive review carried ~10 findings whose priority depends
on whether this harness is a distributable product (installer CLI, OS matrix,
CONTRIBUTING, SBOM/signing, transport abstraction, unified product CLI).

## Decision

Optimise for single-operator macOS use. Keep product-compatible seams —
portable/local configuration split and a root workspace — but defer installer,
cross-platform, supply-chain and contribution-surface work until productisation
is actually pursued.

## Consequences

- Installer, cross-platform, supply-chain and contribution-surface work remains
  deferred to a productisation cycle.
- Portable/local configuration boundaries are owned by the standalone
  scope and invariants and
  architecture assurance
  specifications. The root workspace is implemented by the root `package.json`
  and `package-lock.json`, landed through [PR
  #7](https://github.com/mblauberg/provenant/pull/7). Earlier roadmap labels and
  rationale remain in Git history rather than a live roadmap dependency.
- [Issue #97](https://github.com/mblauberg/provenant/issues/97) and its Project
  Status field own the branch-protection decision, repository-setting state and
  user gate. This ADR authorises no repository-setting mutation.

## Clarification — 14 July 2026

Personal-first also governs skill evidence. A direct request for read-only local
history analysis is sufficient authority to inspect the named histories in
place; it does not need a research-grade receipt, redaction pass, retention date
or small-cell suppression. Raw history is not committed or externally shared.
A compact aggregate or paraphrased response to that human in the same session
is local delivery and needs no second disclosure confirmation. The
product-compatible seam is separate authority for a persistent shared artifact,
raw cross-provider handoff, new audience or external destination, not an unused
generic provider collector.

## Rejected

- Personal-only (rejecting portability outright): forecloses cheap seams.
- Distributable-product-now: inflates the roadmap with no current consumer.
