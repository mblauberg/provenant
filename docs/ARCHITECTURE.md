# Provenant architecture

## Purpose

Provenant coordinates agent work, from a brief through execution, review and
landing. It provides a command line, reusable skills and a local Fabric for
runs, sessions and evidence. Projects retain their own policy and authority.

## System map

- [Run lifecycle](specs/run.md) and [lane lifecycle](runbooks/lane-lifecycle.md):
  launch, observe, recover and settle work.
- [Fabric](specs/fabric.md): coordinate work and retain its records.
- [Routing](specs/routing.md): select execution routes from policy and evidence.
- [Rust workspace](specs/rust-workspace.md): module ownership and layering.
- [Runbooks](runbooks/README.md): operator procedures.

## Trust boundaries

[Global rules](../GLOBAL-AGENTS.md) define authority and user gates.
[Supervisor](specs/supervisor.md) owns writer custody and shared Git metadata;
[worktrees](worktrees.md) describe their use. [Run](specs/run.md) owns lane
confinement and settlement. [Human inbox](runbooks/human-inbox.md) describes
how a chair receives decisions; a message is not a grant of authority.
