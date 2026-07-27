# ADR 0013 — Thin `provenant` CLI for command discovery

**Status:** Accepted 2026-07-18 (user, [issue
#266](https://github.com/mblauberg/provenant/issues/266))

**Date:** 18 July 2026

## Context

Provenant had several stable command entry points but no single discovery
surface. Agents had to know those command names in advance, while a unified
execution layer would have duplicated Fabric and routing ownership.

## Decision

Use a thin installed `provenant` front door for command discovery. It exposes
only the existing harness entry points and transfers control without changing
their contracts:

```text
provenant help
provenant doctor [existing agent-fabric doctor arguments]
provenant route [existing model-route arguments]
provenant worktree [existing worktree arguments]
provenant check [existing check-harness arguments]
provenant fabric [existing agent-fabric arguments]
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

The installed symlink must resolve the wrapper's real checkout without changing
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

The front door must not become another orchestration or provider layer. Defer or
reject the following:

- direct provider execution or normalised provider flags;
- provider fallback, substitution or retry;
- cron, scheduling or a second daemon;
- `run`, `wait`, pane capture or lifecycle state;
- a rewrite of `scripts/check-harness`;
- replacing existing scripts with symlinks or redirecting their callers.

Agent Fabric remains the owner of answer-bearing provider execution, retained
sessions, receipts and communication. `scripts/model-route` remains the owner
of model selection. Herdr and native harnesses retain their existing execution
and observation roles. A thin front door must not reinterpret an error or turn
one provider's failure into another provider's action.

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

1. All five delegated commands execute the documented existing owner.
2. Representative success, usage-error and downstream-failure cases preserve
   stdout, stderr and exit status exactly.
3. The same tests pass from the Provenant root, an unrelated Git repository and
   a non-repository temporary directory.
4. Existing direct command tests and calls remain unchanged.
5. `provenant help` identifies the five behavioural owners and distinguishes
   Fabric clients from providers.

Any expansion beyond these commands or ownership boundaries requires a separate
decision. Usage evidence may justify improving discovery text, but does not by
itself authorise execution, fallback, scheduling or state behaviour here.

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

The thin front door is worthwhile only while it remains a discovery layer with
mechanically verifiable passthrough behaviour. Any new behaviour belongs with
the existing owner or requires a separate design decision.
