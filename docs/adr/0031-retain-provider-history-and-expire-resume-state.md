# ADR 0031: Retain provider history and expire resume state

**Status:** Accepted 2026-10-26, owner-approved in #1570

## Context

A workshop-host inventory found 12 GB under `runs/`, including 11.4 GB of
attempt-private provider state. Codex caches and plugins accounted for about
10.2 GB; transcripts and history databases accounted for about 0.6 GB.
Only continuation reads the private provider state. Telemetry reads the
attempt records and `usage.jsonl`.

ADR 0007 deferred durable-run retention until a concrete requirement
established deletion protections. This inventory supplies that requirement.

## Decision

Replace ADR 0007's deferral for attempt-private provider state with automatic
retention. Keep its decision against universal retention classes and general
state deletion.

At terminal settlement, prune only provider cache paths known to be
regenerable. Codex's initial entry is `cache/`, `plugins/`, `skills/` and
`models_cache.json`. Provider startup fetches or generates them again.
Keep the paths in one small provider table; add another provider only with
evidence that its entries are regenerable.

GC removes known credential copies and links once a terminal attempt is
24 hours old. It expires resumable provider state after
`retention.provider_state_days`, default 7. Both ages start at terminal
`finished_at`. Existing runs use the same rule on the first sweep.

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
