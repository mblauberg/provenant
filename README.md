<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="./assets/brand/provenant-icon-dark.svg">
    <img src="./assets/brand/provenant-icon-light.svg" width="128" height="128" alt="Provenant icon">
  </picture>
</p>

<h1 align="center">Provenant</h1>

<p align="center">
  Run coding agents from the harness you already use, on the subscriptions you already pay for,
  <br>with quota-aware routing, confined worktrees, full run records and messaging between sessions.
</p>

Provenant helps a coding-agent session delegate work to other agents using your
existing provider accounts. It routes work, tracks quota, isolates writers in
Git worktrees and keeps records of runs. The session you started stays in charge
of the work and its review.

A delegated task is a **lane**: an agent working with a defined scope and
access mode. **Fabric** coordinates sessions through messages, tasks and
claims, including across configured hosts. Provenant provides a CLI and skills
that agents can use from their existing harnesses.

## Why Provenant

Coding work rarely follows a fixed sequence. An investigation can change the
implementation, and a review can expose another problem. Provenant lets the
session decide what to delegate and what to do with the result, while the
harness handles routing, execution and records.

Routes select models and accounts with quota headroom. A writer works in its
own registered worktree, so parallel changes have separate Git state. Run
records preserve the work and its outcome for inspection and continuation.
Permission policy and the trust placed in a route matter: review the
[configuration](docs/runbooks/configuration.md#lane-sandbox-mode) and grant
only the authority the work needs.

The optional [macOS quota menu](apps/quota-menubar/README.md) presents account
usage and opens the console. Local telemetry helps inspect usage and runs;
see the [telemetry runbook](docs/runbooks/telemetry.md).

## How it works

The host resolves a route and account, launches the lane and records its
attempt. A writer's work is kept in a separate worktree; the host can verify
and deliver its result for review. Read-only assignments and writing
assignments have different purposes and authority. Check the result and its
review evidence before accepting a change.

Fabric runs alongside the lanes. Sessions can exchange messages, share tasks
and claim work without relying on a single conversation's context. Configured
peer hosts extend that coordination to other machines. Run records make it
possible to inspect the outcome and continue work when an attempt stops.

Routes and account settings can be adapted to your providers and priorities.
The [routing runbook](docs/runbooks/routing-and-templates.md) explains how to
configure them; the [configuration runbook](docs/runbooks/configuration.md)
explains where settings come from. CLI help and the configuration schema are
the reference for the available options.

## Install

Follow the [installation runbook](docs/runbooks/install.md) for requirements,
installation and account setup. Dispatch runs on macOS; Linux supports builds
and CI checks.

## Quick start

After installation, run `provenant help` to explore the CLI and
`provenant help COMMAND` for a command's options. Ask your coding-agent session
to delegate a bounded task, or use the CLI directly. Start with a task whose
output you can review: a lane invokes a real agent under the authority you
provide.

Use `provenant status` to inspect work and its outcome. The
[architecture](docs/ARCHITECTURE.md) explains the lifecycle and boundaries;
the [work-item workflow](docs/runbooks/github-workflow.md) covers changes
intended for a repository.

## Documentation

The [documentation index](docs/README.md) links the architecture, specifications,
decisions and runbooks. [GLOBAL-AGENTS.md](GLOBAL-AGENTS.md) contains the
cross-project rules installed for agent sessions.

## Contributing

Read [AGENTS.md](AGENTS.md) and [MAINTAINING.md](MAINTAINING.md) before making
a change. The [GitHub workflow](docs/runbooks/github-workflow.md) describes
branches, review and landing.

## Security

Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

## Licence

Provenant is available under the [MIT licence](LICENSE). See
[ACKNOWLEDGEMENTS.md](ACKNOWLEDGEMENTS.md) for credits.
