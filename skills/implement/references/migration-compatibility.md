# Migration compatibility

Use for schema, data, protocol, API, configuration, dependency or storage
changes where old and new producers or consumers may coexist.

## Compatibility proof

1. Inventory producers, consumers, persisted data, generated artifacts,
   deployment order and the maximum mixed-version window. State unsupported
   combinations explicitly.
2. Prefer **expand, migrate, contract**: add a backward-compatible shape;
   deploy tolerant readers/writers; backfill or dual-read/write only when
   necessary; prove convergence; then remove the old shape separately.
3. Characterise old data and traffic before editing. Test old-reader/new-writer,
   new-reader/old-writer, retry/replay, partial rollout, interrupted backfill,
   idempotency and failure boundaries according to risk.
4. Give every compatibility adapter, flag, dual path or deprecated field an
   expiry owner, removal condition and latest review date. Deletion requires
   usage-zero evidence across known runtime, build, registry, configuration and
   data paths, not elapsed time or a search result alone.
5. Separate rollback from recovery. Reversal may be unsafe after new writes or
   public consumption; define backup/restore, forward repair, traffic stop,
   feature disablement or containment, with owners and verification. Never
   assume every migration needs a destructive down migration.
6. Bind rehearsal results, data invariants, compatibility matrices and residual
   risk to the enclosing delivery receipt. Promotion remains owned by
   `release`; irreversible data loss or disclosure requires direct user
   authority.

Stop and return to `scope` when the compatibility window, ownership, data-loss
policy or one-way-door decision is unsettled. Use `diagnose` for unexplained
live divergence.

## Research provenance

Independently written synthesis, informed by the migration-safety pattern in
[Addy Osmani's agent-skills at commit 4e8bd9f](https://github.com/addyosmani/agent-skills/tree/4e8bd9fde4a38cd009053e649f4cdc7cd36b568b)
and adapted to this harness's receipt, authority and release gates (reviewed
2026-07-11). No upstream skill text or executable code is imported.
