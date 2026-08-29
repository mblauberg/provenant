# ADR 0013 — Thin `provenant` CLI for command discovery

**Status:** Accepted 2026-07-18 (user, [issue
#266](https://github.com/mblauberg/provenant/issues/266)); amended by [ADR
0020](0020-retire-the-daemon-fabric.md) on 2026-08-02; amended by [ADR
0021](0021-configured-workspace-dispatch-boundaries.md) on 2026-08-29

**Date:** 18 July 2026

> **Cutover note (current reader).** This is a pre-ADR-0020 implementation
> snapshot. Its daemon-era command and provider-execution ownership statements
> are historical evidence, not current operational guidance. Current Fabric
> commands and configuration are owned by
> [`runtime/fabric/README.md`](../../runtime/fabric/README.md); Fabric now
> coordinates messages, tasks and activity while direct CLIs execute providers.

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
provenant doctor [existing agent-fabric doctor arguments]
provenant route [existing model-route arguments]
provenant worktree [existing worktree arguments]
provenant check [existing check-harness arguments]
provenant fabric [existing agent-fabric arguments]
provenant project [existing agent-fabric project arguments]
```

`provenant` owns command names and help text. The existing commands remain the
sole behavioural owners:

| Front-door command | Existing owner |
| --- | --- |
| `doctor` | `scripts/agent-fabric doctor` |
| `route` | `scripts/model-route` |
| `worktree` | `scripts/worktree` |
| `check` | `scripts/check-harness` |
| `fabric` | `scripts/agent-fabric` |
| `project` | `scripts/agent-fabric project` |

The managed resolver must resolve the wrapper's real checkout without changing
the caller's working directory. `doctor` prefixes its arguments with
`scripts/agent-fabric doctor`; `route`, `worktree`, `check` and `fabric` pass
every argument after the subcommand unchanged. Each delegation
must preserve the caller's environment, standard input and signals, preserve
stdout and stderr byte-for-byte, and return the existing command's exit code.

This `doctor` passthrough was added on 27 July 2026 for issue #458 so the
explicit `--consume-provider-quota` opt-in and `--help` remain reachable from
the installed front door. The wrapper still does not interpret those arguments;
Agent Fabric remains their behavioural owner.

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
client connects to Fabric's MCP surface to coordinate work. A provider is an
execution adapter that Fabric can select for an answer-bearing action. Global
instructions or an installed CLI establish neither role on their own.

[Issue #264](https://github.com/mblauberg/provenant/issues/264) established
update-tolerant provider admission by identity and interface contract rather
than executable version or hash. The front door does not alter that policy or
establish client/provider activation. `provenant help` must distinguish the
roles and must not present an installed, configured or proposed integration as
active.

## Accepted slice and expansion gate

The accepted slice is one installed shell wrapper plus focused contract tests.
It does not modify the existing commands or their callers.

The slice is required to retain these measurements:

1. All six delegated commands execute the documented existing owner.
2. Representative success, usage-error and downstream-failure cases preserve
   stdout, stderr and exit status exactly.
3. The same tests pass from the Provenant root, an unrelated Git repository and
   a non-repository temporary directory.
4. Existing direct command tests and calls remain unchanged.
5. `provenant help` identifies the six behavioural owners and distinguishes
   Fabric clients from providers.

`project` is part of the managed resolver's accepted command set and delegates
to `scripts/agent-fabric project`. Legacy symlinks are migration inputs only.
Any further expansion beyond these commands or ownership boundaries requires a
separate decision. Usage evidence may justify improving discovery text, but
does not by itself authorise execution, fallback, scheduling or state behaviour
here.

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
