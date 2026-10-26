# ADR 0025: Verified upstream lineage for brokered providers

**Status:** Accepted

Changed: 2026-10-26, #1754.

## Context

A broker or adapter can carry models from several vendors. Treating the pipe
as the model family can both reject valid independent work and certify a
family distinction that has not been established.

## Decision

Model family means upstream answering-model lineage, separate from adapter or
endpoint identity. Assurance consumes verified lineage evidence, not a broker's
name or an unverified model slug. Unknown and generic broker routes may execute
ordinary work but supply no distinct-family assurance.

The providers and routing crates bind family evidence to the catalogue revision
and provider observation. A changed or conflicting route cannot reuse stale
family proof. Keep the resolved route and evidence in the attempt record;
never turn transport diversity into an independence claim.

## Consequences

Multi-family pipes remain usable while assurance fails closed on unresolved
lineage. The Rust routing owner replaces the retired slug-inference fields and
allowlists; the [routing design](../specs/routing.md) owns the evidence contract.
