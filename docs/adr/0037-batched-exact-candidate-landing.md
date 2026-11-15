# ADR 0037: Batched exact-candidate landing

**Status:** Accepted

## Context

Checking individual pull request heads does not check their combination with the target. Updating each branch serialises independent changes and repeats CI. Review and lineage must remain bound to approved commits.

## Decision

Gate the exact integration candidate before publication. Batch eligible changes by default, preserving reviewed heads or recording their mapping when publication rewrites them. Recheck authority and rebuild the candidate when its basis moves; isolate failures rather than treating a partial batch as accepted. Recovery must reconcile uncertain publication before repeating an effect.

A single repository CI observer serves landing and status consumers. Accepted delivery receipts may provide approval through the same approval boundary as forge review, subject to proof that the reviewed subject still covers the candidate.

The [landing runbook](../runbooks/github-workflow.md#land) owns the procedure and the [CLI spec](../specs/cli.md#landing-adds-no-merge-authority) owns the authority contract.

## Alternatives and consequences

Updating every branch adds serial churn. A forge queue as the default ties integration to that queue; squash by default obscures reviewed commit lineage. Candidate gating costs an integration build but checks what is intended for publication.
