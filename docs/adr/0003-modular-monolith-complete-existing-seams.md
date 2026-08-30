# ADR 0003 — Modular monolith; complete existing seams

**Status:** Accepted 2026-07-13 (human ratification of chair + codex-pair calls)

> **Current-reader note.** ADR 0020 retired the daemon and
> `runtime/agent-fabric`; current coordination lives in `runtime/fabric`. The
> retained rule is to complete proven seams and avoid speculative scaffolding;
> paths and implementation detail below are historical.

## Context

At the time of this decision `runtime/agent-fabric/src/core/fabric.ts` was a
7,401-line class owning policy,
SQL, provider execution, recovery, budgets, barriers, projections, task/message
orchestration and capability issuance. The review pack proposed decomposition
behind a `FabricRuntime` composition root with a generic command dispatcher,
`UnitOfWork` and domain-event framework. The codex-pair challenge showed the
refactor is already part-done: `CommandJournal` (explicit SQLite transactions,
idempotent results), `ProviderSessionCoordinator`, focused stores/services and
`ExternalEffectService` custody exist today.

## Decision

Keep one process and one transactional SQLite authority. Decompose `Fabric`
internally by **completing the existing seams**, extracting one coherent
vertical slice at a time by change pressure — provider payload admission first
(it also serves ADR 0002 step 2). Do not pre-install generic scaffolding
(universal UnitOfWork, dispatcher, domain-event framework); add an abstraction
only when two extracted slices need it or a testable invariant requires it.
Preserve direct SQL in focused stores. Keep the façade until callers move; no
parallel second implementation.

## Consequences

- Characterisation and recovery tests guard every extraction.
- Import-boundary/architecture tests (F-033 test
  and edge golden)
  precede extraction.
- Extraction after the write-pilot steps uses the merged
  `ProviderActionDispatchInputV1` contract shape.

## Rejected

- Microservices / distributed workflow engine (no demonstrated need).
- Provider-native-only coordination without Fabric (loses neutral authority).
- MCP as the whole control plane (wrong centre of gravity).
- Pack's full scaffolding-first target architecture (risks moving complexity
  behind more names).
