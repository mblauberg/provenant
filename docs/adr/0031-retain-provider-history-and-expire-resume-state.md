# ADR 0031: Retain provider history and expire resume state

**Status:** Accepted

Changed: 2026-10-26, #1754.

## Context

Attempt-private caches can dominate storage, while transcripts and durable
results must survive cache collection. Only continuation reads resumable
provider state; telemetry uses attempt records independently.

## Decision

Apply automatic retention to attempt-private provider state.
[ADR 0007](0007-defer-universal-retention-and-deletion.md) owns disposal
custody; it grants no general state-deletion authority.

At terminal settlement, prune only provider cache paths known to be
regenerable. The provider-owned cache inventory identifies
paths that startup can fetch or generate again; add another provider only with
evidence that its entries are regenerable.

GC removes known credential copies and links once a terminal attempt is
24 hours old. It expires resumable provider state after
`retention.provider_state_days`, default 7. Both ages start at terminal
`finished_at`.

Before expiry, archive transcripts and history databases under the attempt's
`provider-history/`. Codex sessions, history and their database companions,
and Claude's private project sessions survive. Until another provider's
transcript layout is established, archive its unclassified payload
conservatively after removing known credentials. Do not guess that unknown
files are disposable.

Keep the result, captures, exit evidence, records and digests. Seal an expiry
marker so resume explains that state has expired before consuming a
continuation. Pending continuations protect their source state.

Use the existing no-follow directory operations, tombstone rename and
identity-checked payload disposal. GC takes locks without waiting, skips
retained or unreadable reservations, and touches only indexed terminal
attempts. Dry runs preview removals. Settlement failures are retried by GC.

## Consequences

- Closed runs shed Codex caches immediately and have a bounded resume window.
- Credential expiry happens on the first eligible sweep after 24 hours;
  live or unobservable custody prevents collection.
- Transcripts remain available after resume expires. Unclassified providers
  may retain more history until their transcript layout is established.
- There is no schema migration or general deletion framework.
- Results and telemetry remain available independently of provider state.

Overturn when: a reader other than continuation needs resumable provider state.
