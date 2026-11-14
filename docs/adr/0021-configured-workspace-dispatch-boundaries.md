# ADR 0021: Crate layering, one execution owner and delegation bounds

**Status:** Accepted

## Context

One-way dependencies keep policy testable without the command layer and prevent a second scheduler or lifecycle store.

## Decision

Compose one Rust executable from libraries with one-way, enforced dependency edges. Each execution concern has one owner; the CLI composes those owners rather than owning a second launch path. Delegation may narrow authority, never widen it ([ADR 0035](0035-four-hard-boundaries-and-flexible-everything-else.md), [ADR 0040](0040-open-lane-policy-with-explicit-boundaries.md)). External provider, hook and skill contracts remain theirs.

The [workspace spec](../specs/rust-workspace.md#crate-layering) owns the layering contract; [ADR 0028](0028-rust-port-dependencies.md) owns dependency admission.

## Consequences

A new case should extend the owning mechanism through a clear interface. Generalise for foreseeable change when the abstraction makes change easier; avoid indirection whose cost exceeds that benefit. Provider success alone does not establish delivery acceptance.
