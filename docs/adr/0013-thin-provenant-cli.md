# ADR 0013: Thin CLI composition edge

**Status:** Accepted

Changed: 2026-10-26, #1754.

## Context

Operators need one command-discovery surface. Putting execution policy into
that front door would create a second owner for routing, process custody and
delivery evidence.

## Decision

`provenant` is the installed Rust command and composition edge. The CLI crate
owns arguments, help and bounded rendering; domain crates own behaviour.
The executable wires those owners together rather than implementing another
provider adapter, scheduler, store or lifecycle ledger.

CLI and MCP calls reach the same run and Fabric owners. A harness may be a
Fabric client without being an enabled execution provider. Installation or
configuration alone proves neither activation nor successful execution.

Discover current commands through `provenant help`; command inventories belong
to code and generated reference, not this record. The
[workspace design](../specs/rust-workspace.md) owns crate layering.

## Consequences

One memorable entry point preserves domain ownership. New command surfaces
need an owning crate and explicit behaviour; convenience does not authorise
silent fallback, new state or wider provider access.
