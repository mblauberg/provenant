# Architecture decision records

Decisions ratified by the user on 2026-07-13. Each ADR is the canonical owner
of its decision; supporting scoping evidence, challenge history and rejected
alternatives remain available in Git history without forming a live
cross-reference dependency. ADRs 0009–0010 were ratified later, on 2026-07-15;
ADRs 0011–0012 were ratified on 2026-07-16; ADR 0013 was ratified on
2026-07-18; ADR 0014 was ratified on 2026-07-20; ADRs 0015–0016 were ratified
on 2026-07-21; ADRs 0017–0018 were ratified on 2026-07-28; ADR 0019 was
ratified on 2026-07-30 and amended on 2026-07-31; ADR 0020 was ratified on
2026-08-02; ADR 0021 was ratified on 2026-08-29. The user-approved 2026-08-31
issue-native amendments to ADRs 0011 and 0017 in issue 711 were noted as
consistent with ADR 0009.
Pre-ADR-0020 implementation
paths in retained decisions are
historical evidence; the current Fabric owner is
[`runtime/fabric/README.md`](../../runtime/fabric/README.md).

| ADR | Title | Status |
|---|---|---|
| [0001](0001-personal-first-product-compatible.md) | Personal-first, product-compatible posture | Accepted |
| [0002](0002-capability-compiled-execution-authority.md) | Capability-compiled execution authority (write profiles) | Superseded by ADR 0020 |
| [0003](0003-modular-monolith-complete-existing-seams.md) | Modular monolith; complete existing seams | Accepted; daemon-era implementation historical after ADR 0020; anti-scaffolding principle retained |
| [0004](0004-per-domain-truth-owners.md) | Per-domain truth owners, no god manifest | Accepted |
| [0005](0005-lifecycle-kernel-extends-delivery.md) | Lifecycle kernel extends the delivery kernel | Accepted (amended 2026-07-15) |
| [0006](0006-defer-backlog-contract.md) | Defer a canonical backlog contract | Superseded by ADR 0011 |
| [0007](0007-defer-universal-retention-and-deletion.md) | Defer universal retention classes and typed deletion | Accepted (amended 2026-07-15) |
| [0008](0008-review-pressure-risk-and-oracle-adjusted.md) | Risk/oracle-adjusted certifying review | Accepted; unimplemented follow-up superseded 2026-07-15; amended 2026-07-16; addendum 2026-07-22 |
| [0009](0009-standalone-semantic-specifications.md) | Standalone semantic specifications | Accepted (amended 2026-07-28 by ADR 0017; 2026-08-31 issue-native amendment noted) |
| [0010](0010-lifecycle-receipt-authority-distinct-trust-boundary.md) | The lifecycle receipt authority is a trust boundary distinct from provider authority compilation | Accepted; daemon-era implementation historical after ADR 0020; vocabulary boundary retained |
| [0011](0011-github-owns-work-state.md) | GitHub owns current work state | Accepted (user amendment 2026-08-31 via issue 711) |
| [0012](0012-defer-bun-node-pinned-runtime.md) | Defer Bun; Node stays the pinned runtime family | Accepted; daemon analysis historical after ADR 0020 |
| [0013](0013-thin-provenant-cli.md) | Thin `provenant` CLI for command discovery | Accepted; current command map amended by ADR 0020 and amended by ADR 0021 |
| [0014](0014-comparative-skill-evals-on-suspicion.md) | Comparative skill evals run on suspicion, not by default | Accepted |
| [0015](0015-bootstrap-paired-task-completion-evidence-bound-reply.md) | Bootstrap paired-task completion via an evidence-bound reply, not authority widening | Superseded by ADR 0020 |
| [0016](0016-gate-b-restate-permanent-by-design.md) | Keep GATE-B' skipped by design after the RESTATE decision | Accepted |
| [0017](0017-specifications-own-non-derivable-intent.md) | Specifications own non-derivable intent only | Accepted (user amendment 2026-08-31 via issue 711); daemon-era schema example historical |
| [0018](0018-accept-portal-stdio-v1-launch-custody.md) | Accept `portal-stdio-v1` as review-portal launch custody | Superseded by ADR 0020 |
| [0019](0019-installed-file-class-ownership.md) | Installed file-class ownership by product, instance, or seeded template | Accepted (amended 2026-07-31; ADR 0020 cutover note) |
| [0020](0020-retire-the-daemon-fabric.md) | Retire the daemon fabric for a daemonless SQLite bus | Accepted |
| [0021](0021-configured-workspace-dispatch-boundaries.md) | Configured-workspace provider access and dispatch boundaries | Accepted |
