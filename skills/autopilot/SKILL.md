---
name: autopilot
description: "Use for a large, open-ended mission that must continue across sessions until the user writes STOP. Use orchestrate for bounded waves and the owning lifecycle skills for decisions and changes. Not for bounded PR-gate delivery."
---

# Autopilot

Autopilot guides a long-running mission. It delegates bounded work to
`orchestrate` and decisions or software changes to their owning skills. The run
and Fabric own task state, execution, admission, and recovery; this skill keeps
the mission goal and user authority clear. It does not create a second queue,
lease ledger, or run journal.

The mission remains open until the user sets `STATUS: STOP` in its goal. An
empty lane list or an idle run means there is no eligible work now; it does not
mean the mission is complete. Record what event can resume work, end the current
invocation, and leave the mission open.

## Entry gate

Use autopilot only when the work is open-ended, spans multiple work waves or
sessions, and the user has set an explicit STOP gate. Otherwise use a bounded
skill such as `orchestrate`, `implement`, or `deliver`.

Before proceeding, read the goal, current project instructions, scope and
authority, and the current owner records. Stop for missing authority or a
one-way-door decision. Keep other in-scope work moving where it is independent.

## Operating loop

1. Check the user-owned goal and STOP status. Reopen the current scope and
   authority sources.
2. Run `provenant reconcile` after an interruption or when task state may be
   stale. Inspect `provenant lanes` and the relevant Fabric tasks before
   selecting work.
3. Dispatch bounded work through `skills/orchestrate/scripts/dispatch_run.py` and `orchestrate`.
   Use Fabric tasks for assignment, coordination, and results. `orchestrate`
   owns routing, topology, and review requirements.
4. Inspect returned work with `provenant output`; verify artifacts and checks
   before treating a task as complete. Route changes through `implement` or
   `deliver` as required.
5. Respect bounded retry and escalation rules from the owning lifecycle. Do
   not bypass a user gate, widen the build ceiling, or infer authority from a
   worker's result.
6. When work is interrupted, use `provenant resume` and `provenant reconcile`
   with the run's existing identity. Preserve partial outputs and verify the
   target before retrying.
7. Before compaction or a substantial handoff, save a bounded Markdown
   checkpoint for the actual host session with
   `provenant workflow session save <session-id> --file <checkpoint.md>`.
   Include authority/evidence references, verified progress, next actions,
   blockers, and a resume trigger.
8. If the run has no eligible or active work, follow run admission and pause
   behavior. Report the waiting condition and resume trigger. Do not label the
   mission complete or change `STATUS`; only the user can stop it.

## Mission evidence and STOP

Keep durable project knowledge in its existing owner. Run lanes and Fabric
retain live task state and results; a session checkpoint carries only the
context needed to resume one host session. SessionStart injects only the
checkpoint for its exact key, or an explicit missing/stale status. PreCompact
reports whether that checkpoint needs saving. Neither hook expands authority
or selects a latest file.

When the user sets `STATUS: STOP`, reconcile current work, verify the outcome,
and provide a concise handoff with completed evidence, remaining work, and any
unresolved gate. Do not close the mission because the current frontier is dry.
External actions retain their own user gates.

## References

Read [state-contract.md](references/state-contract.md) for state ownership,
[operating-loop.md](references/operating-loop.md) for the run/Fabric work loop,
[recovery-and-cadence.md](references/recovery-and-cadence.md) for interruption
and idle handling, and [assumptions-and-steering.md](references/assumptions-and-steering.md)
for decisions made within an approved authority envelope.
