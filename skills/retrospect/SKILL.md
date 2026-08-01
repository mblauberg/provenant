---
name: retrospect
description: "Use after delivery, release, incident, evaluation, or a long run to derive evidence-backed process improvements and regression gates. Not for session cleanup, one skill audit, or an active defect; use session, skill-craft, or implement."
---

# Retrospect

Turn completed-cycle evidence into a better next cycle: benchmark, diagnose,
propose, verify, monitor. No retrospective theatre or one dated log per run.
Scale depth to risk/friction: clean routine cycles may return `no change`;
substantial runs, escaped defects and repeated human corrections need the full pass.

## Evidence boundary

Start from approved spec, run receipts, checks/evals, review/repair history,
human corrections, escaped defects, production observations, routing failures,
resource use and retained artifacts. Use only authorised sources. Mark absent
dimensions `unknown`; never infer success from a clean final answer.

For substantial+ or repeated cycles, create `RETROSPECT.json` from the
[template](templates/RETROSPECT.template.json) and validate with
`scripts/validate_retrospect.py`. It is evidence, not diary/project truth.

## Review dimensions

- outcome and acceptance-criteria success;
- trajectory compliance, safety and escaped defects;
- human attention/corrections/rework, gate latency, unnecessary interruption and
  blocked time;
- test/eval and reviewer effectiveness;
- orchestration, model routing, delegation, tool reliability;
- context size, compaction, handoff and artifact hygiene;
- skill triggering, usefulness, overlap and missing capability;
- documentation, project memory, canonical-state freshness;
- cost and latency when receipts contain trustworthy measures.

## Flywheel

1. **Benchmark** against declared acceptance criteria/eval thresholds, baseline
   and comparable runs. Separate output quality from trajectory.
2. **Diagnose** recurring failures by root-cause cluster: product/code,
   specification, test/eval, skill, instructions, routing/adapter, tool,
   context/memory, documentation or authority/process.
3. **Propose** the smallest change for each supported cluster. Each proposal
   names evidence, owner, risk, destination and success measure.
4. **Verify**: turn representative failures into deterministic tests/versioned
   eval cases; run regressions before claiming improvement.
5. **Monitor** the next comparable cycle for recurrence, regressions, cost and
   new failure modes. Feed supported attention/gate changes into the next scope
   cycle, never a parallel process diary.

`improved` requires authorised intervention, passing regression gate and enough
comparable later cycles meeting predeclared target/guard metrics.
Underpowered or confounded evidence is `inconclusive`.

Route unclear intent to `scope`, deterministic defects to `implement`,
stochastic behaviour to `evaluate`, skill evidence to `skill-craft`, and
context cleanup to `session` or `engineering-docs`.

## Learning and authority

Promote durable conclusions to their canonical owner: spec/ADR, runbook,
project instructions, state/context digest, test/eval fixture, skill or routing
policy. Merge with existing truth; never append a parallel diary. Project facts
never live only in private memory; cross-project preferences follow harness
memory policy.

This skill is proposal-first and read-only by default. Apply project/global
harness changes only under explicit authority or an enclosing `implement` run;
material cross-project changes return through user-approved scope. Finish with
a table: finding, evidence, root cause, change, regression gate,
owner/destination, status (`promote`, `experiment`, `defer`, `reject`).

## Adapter-absent path

Without optional Console, Herdr or GitHub, use canonical project artifacts and
emit the skill-owned kind in
[portable-workflow.v1.json](portable-workflow.v1.json). It proves context
existence only, not retrospective evidence. Keep context separate. Improvements
require authority.
