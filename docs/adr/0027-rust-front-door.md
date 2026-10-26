# ADR 0027: One Rust binary and domain-owned workspace

**Status:** Accepted

Changed: 2026-10-26, #1754.

## Context

The two-layer Python front door paid interpreter startup on every call. Keeping
legacy owner scripts beside Rust replacements also created duplicate contracts
and transition machinery without a consumer that needed compatibility.

## Decision

Ship one Rust binary at `tools/front-door`, composed from focused domain crates.
The CLI parses and renders; domain crates own behaviour and may depend only on
lower layers allowed by the [workspace design](../specs/rust-workspace.md).
CLI and MCP calls reach the same run and Fabric owners; the executable adds no
provider adapter, scheduler, store or lifecycle ledger. Complete existing seams under change pressure rather than adding a universal
dispatcher, unit-of-work or event framework in advance of a concrete need.

A harness may be a Fabric client without being an enabled execution provider;
installation or configuration alone proves neither activation nor successful
execution. Discover commands through `provenant help`, not this record.

Rust is the sole runtime for Provenant-owned execution. No Python/TypeScript
fallback, selector, dual parser or legacy-state negotiation is retained.
External provider interfaces, hook protocols, skill format and MCP remain
contracts owned by those external tools.

Release identity covers the workspace sources, manifests, lockfile and assets.
The config crate stamps and installs digest-addressed releases. Current state
lives under the canonical state root; legacy roots are not written.

## Consequences

There is one implementation per subsystem. Rollback uses Git and a matching
installed release, rather than keeping dormant execution paths. Build and
installation operations follow the [install runbook](../runbooks/install.md).
[ADR 0028](0028-rust-port-dependencies.md) owns the dependency trade-off.
