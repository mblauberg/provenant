# ADR 0007: Deletion requires custody of disposable state

**Status:** Accepted

## Context

Temporary payloads and caches need collection without deleting live work or durable results. Provider transcripts remain useful after resumable state expires.

## Decision

Keep durable results unless a separately approved retention rule applies. Collect disposable state only with verified custody and writer exclusion; unknown custody preserves the data for a later retry. Stable locks remain outside disposable trees so their identity cannot be split by deletion.

One declarative retention table names artefact classes, their owners and retention policy. Each owner performs its own disposal. Retain provider history when expiring resumable state, protect pending continuations, and treat unclassified provider payloads conservatively. Collect only provider caches known to be regenerable.

The [GC runbook](../runbooks/gc.md#what-is-removed-and-when) owns procedures and configured retention.

## Consequences

Collection can reclaim disposable data without making a cache into the authority for results or telemetry. Conservative handling may retain extra provider data until its classification is known.
