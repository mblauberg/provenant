# Handoffs

## Active

[Provenant stable-baseline chair handoff](HANDOFF-2026-08-01-provenant-baseline-chair.md)
is active while the fused baseline, Agent Fabric recovery and repository-split
acceptance work continue.

[GitHub Issues](https://github.com/mblauberg/provenant/issues) own the current
owner, dependencies and user gates; Project Status owns workflow state. Read
the relevant issue, its linked specification and ADRs, then inspect live
repository and runtime state.

Dated handoffs through 15 July 2026 were consumed during
[W012 reconciliation](https://github.com/mblauberg/provenant/issues/23); the
16 July README-rewrite handoff was consumed when commit `7efc266` landed the
rewrite. The 18 July consolidated-CLI handoff was consumed by issue #266; its
durable decision is [ADR 0013](../adr/0013-thin-provenant-cli.md). The 20 July
disclosure-refactor handoff was consumed by the
[#335](https://github.com/mblauberg/provenant/issues/335) implementation train;
its durable authority is the spec
[`harness/disclosure-refactor.md`](../specs/harness/disclosure-refactor.md) and
[ADR 0014](../adr/0014-comparative-skill-evals-on-suspicion.md). All remain
available in Git history. A new handoff is only for live session or run
continuity: it must name its current issue or run and be removed when consumed.

For historical W010 evidence, start with the canonical [capability-compiled
execution authority route](../efforts/EFFORT-capability-profiles.md), then
inspect completed [issue #22](https://github.com/mblauberg/provenant/issues/22).
That route links the governing ADR, standalone specifications and related work
items without recording their state.
