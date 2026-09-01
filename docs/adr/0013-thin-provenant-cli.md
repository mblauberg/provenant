# ADR 0013 — Thin `provenant` CLI for command discovery

**Status:** Accepted 2026-07-18 (user, [issue
#266](https://github.com/mblauberg/provenant/issues/266)); amended by [ADR
0020](0020-retire-the-daemon-fabric.md) on 2026-08-02; amended by [ADR
0021](0021-configured-workspace-dispatch-boundaries.md) on 2026-08-29; amended
by [ADR 0022](0022-thin-fabric-mcp-execution-facade.md) on 2026-09-01

**Date:** 18 July 2026

> **Cutover note (current reader).** This is a pre-ADR-0020 implementation
> snapshot. Its daemon-era command and provider-execution ownership statements
> are historical evidence, not current operational guidance. Current Fabric
> commands and configuration are owned by
> [`runtime/fabric/README.md`](../../runtime/fabric/README.md); Fabric now
> coordinates messages, tasks and activity, and its thin MCP façade can start
> the same provider owners available through the direct CLI.
> The current front door delegates `route`, `worktree`, `check`, `fabric`,
> `dispatch`, `batch` and `run` to the owners below. The original daemon-era
> command table remains in Git history.

## Context

Provenant had several stable command entry points but no single discovery
surface. Agents had to know those command names in advance, while a unified
execution layer would have duplicated Fabric and routing ownership.

## Decision

Use a thin managed `provenant` front door for command discovery. It exposes
the existing harness entry points and transfers control without changing their
contracts:

```text
provenant help
provenant route ...
provenant worktree ...
provenant check ...
provenant fabric ...
provenant dispatch ...
provenant batch ...
provenant run ...
```

`provenant` owns command names and help text. The existing commands remain the
sole behavioural owners:

| Front-door command | Existing owner |
| --- | --- |
| `route` | `scripts/model-route` |
| `worktree` | `scripts/worktree` |
| `check` | `scripts/check-harness` |
| `fabric` | `runtime/fabric/bin/fabric` |
| `dispatch` | `skills/orchestrate/scripts/dispatch_run.py` |
| `batch` | `skills/orchestrate/scripts/batch_run.py` |
| `run` | `skills/orchestrate/scripts/run_controls.py` |

The managed resolver finds the product checkout without changing the caller's
working directory. Each delegation preserves arguments, standard input,
signals, stdout, stderr and the owner's exit code. The `check` command also
selects the caller's registered checkout and clears Git redirect variables so
the gate cannot certify another checkout. Direct `scripts/check-harness`
clears the redirects and reports its configured or local root, but does not
perform caller-checkout selection. Component-root substitutions require an
explicit test-only opt-in, and the gate reports those substitutions or an
explicit `HARNESS_PYTHON`. Other commands preserve the environment.

This gives agents one memorable discovery surface while keeping current scripts
stable for automation and direct use.

## Boundary

The front door must not duplicate orchestration or provider-adapter mechanics.
ADR 0021 amends this boundary to permit bounded dispatch and batch commands
whose behavioural owner is the orchestration adapter layer. Defer or reject the
following:

- a second provider execution implementation or normalised flags that bypass
  the existing adapter owner;
- implicit provider fallback, substitution or retry;
- cron, scheduling or a second daemon;
- an independent lifecycle database or competing delivery receipt;
- a rewrite of `scripts/check-harness`;
- replacing existing scripts with symlinks or redirecting their callers.

The front door may expose bounded `dispatch`, `batch` and `run` inspection
commands, but those commands delegate to the one orchestration dispatch owner.
They must not reinterpret an error or turn one provider's failure into another
provider's action without an explicit retry or routing decision. See ADR 0021
for the configured-workspace access, compact dispatch-manifest and assurance
boundaries.

## Clients and providers

A harness can be a **Fabric client** without being a **Fabric provider**. A
client connects to Fabric's MCP surface to coordinate work. The orchestration
adapter layer and direct official provider CLIs own answer-bearing execution.
Fabric MCP may start those existing owners through the thin façade accepted in
ADR 0022, but does not select routes or implement providers. Any retained
pre-ADR-0020 statement that Fabric owned provider execution is historical
context, not current operational guidance. Global instructions or an installed
CLI establish neither role on their own.

[Issue #264](https://github.com/mblauberg/provenant/issues/264) established
update-tolerant provider admission by identity and interface contract rather
than executable version or hash. The front door does not alter that policy or
establish client/provider activation. `provenant help` must distinguish the
roles and must not present an installed, configured or proposed integration as
active.

## Accepted slice and expansion gate

The accepted slice is one installed shell wrapper plus focused contract tests.
It does not duplicate the existing commands.

The slice is required to retain these measurements:

1. All seven delegated commands execute the documented existing owner.
2. Representative success, usage-error and downstream-failure cases preserve
   stdout, stderr and exit status exactly.
3. The same tests pass from the Provenant root, an unrelated Git repository and
   a non-repository temporary directory.
4. Existing direct command tests and calls remain unchanged.
5. `provenant help` identifies the behavioural owners and distinguishes
   Fabric clients from providers.

Any expansion beyond these commands or ownership boundaries requires a separate
decision. Usage evidence may justify discovery text, but does not itself
authorise execution, fallback, scheduling or new state behaviour.

## Alternatives and trade-offs

**Keep the existing scripts only.** This has no new maintenance cost and keeps
ownership obvious, but agents must already know several command names.

**Add documentation without a command.** A short command index is cheaper than
a wrapper, but the accepted slice keeps the installed front door discoverable
from any working directory.

**Build a unified execution CLI.** Normalised launching, fallback, scheduling
and waiting look convenient, but duplicate Fabric, routing and Herdr semantics.
They also create a second place for authority, receipts, provider differences
and failure handling. The reviewed proposal rejects this option.

The thin front door remains worthwhile as a discovery layer and operator entry
point. New bounded behaviour belongs with the existing dispatch owner and must
be mechanically verifiable; it must not create a second owner.
