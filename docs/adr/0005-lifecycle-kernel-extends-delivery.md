# ADR 0005 — Lifecycle kernel extends the delivery kernel

**Status:** Accepted 2026-07-13; amended 2026-07-15

> **Current-reader note.** "Project kernel decisions into Fabric" below is
> daemon-era. ADR 0020 retired that projection and ADR 0021 confined Fabric to
> coordination and ordinary dispatch, so Fabric enforces no lifecycle rule.
> `deliver` and the canonical `delivery-run` receipt are the current lifecycle
> authority. The rest of this decision stands.

## Context

Lifecycle, review and authority rules are repeated across `HARNESS.md`,
skills, specs, validators and runbooks (F-004). The pack proposed a new
executable lifecycle policy engine. The repo already has an executable neutral
kernel: `delivery-run` schema v1, `config/delivery-profiles.json` and
`skills/deliver/scripts/validate_delivery.py` (risk floors, human-evidence
gates, authority containment).

## Decision

Extend the existing delivery kernel rather than build a second policy model.
Make only objectively decidable minima executable: risk floor, authority
containment, profile admission, required evidence/gates, review independence,
repair ceiling and effect/release gates. Project kernel decisions into Fabric.
Judgement-bearing choices (whether ambiguity warrants scoping, context
staleness, human acceptance) stay with the chair and skills. Skills reference
the kernel instead of restating gates.

Each delivery artifact keeps its existing `retention` policy field, constrained
by project and risk profile policy. A universal retention-class vocabulary is
not a required executable minimum.

## Consequences

- `HARNESS.md` stays a short constitution.
- Amendment-history cleanup follows the [standalone semantic specification
  decision](0009-standalone-semantic-specifications.md); current contracts are
  discovered through the [specification index](../specs/README.md).

## Risks and controls

| Risk | Control |
|---|---|
| Kernel becomes a bloated mega-skill | Stable state machine only; profiles and references hold depth. |
| Generic gates weaken domain requirements | Profiles may strengthen only; high-stakes release is always user action. |
| Receipt ceremony overwhelms small tasks | Risk threshold; routine one-shot work may use an ephemeral receipt or none. |
| Local history escapes its requested scope | Read in place, never commit raw history and keep sharing/export behind explicit destination and content authority. |
| Evals optimise to their own fixtures | Conditional held-outs, repeated trials, mixed graders and user calibration. |
| Multi-agent cost exceeds value | Decomposability gate, one writer and proportional lanes. |
| Concurrent agent-fabric work conflicts | Fabric coordinates; one writer and explicit write partitions protect shared surfaces. |
| Research becomes stale | Dated evidence cut-off, 90-day decision expiry and retrospective refresh proposal. |
