# Agent continuity and checkpointing

Status: Historical pre-ADR-0020 research reference; not current implementation
authority

Current owners: [ADR 0020](../adr/0020-retire-the-daemon-fabric.md),
[`runtime/fabric/README.md`](../../runtime/fabric/README.md), and
[`HARNESS.md`](../../HARNESS.md). Daemon, lease and Console claims below are
retained as dated evidence only.

Evidence snapshot: [July 2026 continuity and routing evidence](evidence-snapshots/agent-continuity-routing-2026-07.md)

Normative owners: lifecycle and gates,
recovery and reconciliation, and
intake and fresh-context continuation

## Conclusions

- Repository artifacts and daemon-owned revisions preserve truth. Provider
  transcripts, summaries and compact blobs are context aids, not canonical
  project memory.
- A policy rotation is a genuinely fresh provider context. Attaching or
  resuming the same full history is crash recovery and must not be relabelled
  rotation.
- Lifecycle custody is a two-phase, generation-fenced action: quiesce and bind
  the canonical checkpoint; perform at most one provider effect; attest and
  atomically adopt; otherwise prove no effect, supersede, quarantine or abandon.
- The daemon derives task, authority, lease, mailbox, child, open-work,
  repository, artifact and evidence values. A model may add narrative but
  cannot author its own custody facts.
- Parent and child continuity are independent. A native child is independently
  recoverable only with a stable contract-tested identity bridge; otherwise the
  native graph is one opaque bounded task.
- Context pressure is distinct from cumulative spend. Missing current-window
  evidence stays unknown and never becomes a fabricated percentage.
- Ambiguous provider effects remain attached to the original stable action and
  recovery owner. Lookup precedes any retry; no path blindly replays compact,
  spawn, attach or promotion.

At the snapshot date, these conclusions were embodied in the lifecycle state
machine and persistence owner. They did not imply an automatic pressure
controller or threshold policy.

## Evidence

| Evidence | Durable lesson | Limits |
|---|---|---|
| Google ADK pause/resume guidance | Durable state, external wake-up and hydration should be tested as one lifecycle. | Another framework is not a state owner here. |
| Parallel Context Compaction | Keep structured checkpoint truth independent of generated summary/compression technique. | Reported results are workload-specific. |
| Anthropic containment engineering | Prefer hard custody/semantic gates over repetitive approval prompts. | Vendor engineering evidence, not a local conformance result. |
| Current lifecycle design | Generation high-water, checkpoint binding, fresh rotation, successor attestation and recovery already have one owner. | Implementation still requires full crash/conformance gates. |
| Console fresh implementation rule | Substantial work starts from an accepted digest-bound handoff in a fresh context. | Routine bounded work may remain in the current context. |

Source URLs and dated caveats are preserved in the evidence snapshot rather
than repeated as volatile current-state claims.

## Unknowns

- Automatic pressure thresholds, reserves, hysteresis and minimum intervals.
- Maximum in-place compactions by role/task class.
- Provider-specific pre-limit signal quality and endpoint-specific windows.
- Whether any native child identity bridge is stable enough for independent
  lifecycle custody on each pinned host version.
- Calibration of proactive chair rotation versus task-bound fresh workers.

Unknowns remain nonbinding. In particular, an `unknown` pressure reading does
not by itself authorise cancellation, rotation or a new provider action.

## Refresh triggers

Refresh after a pinned Codex/Claude compaction or resume wire change, a new
adapter context signal, a lifecycle crash/recovery failure, evidence of
checkpoint omission, a changed child-identity contract, or a material context
compaction study. Reopen the live specs/runtime and current official provider
documentation before asserting present wire events or context limits.
