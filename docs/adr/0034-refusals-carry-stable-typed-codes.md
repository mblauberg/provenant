# ADR 0034: Refusals carry stable typed codes

**Status:** Accepted

## Context

Matching error prose obscures causes and makes harmless wording changes break callers.

## Decision

Each refusal has a stable typed cause declared by its owner. Causes survive crate and checkpoint boundaries; diagnostics disclose only facts safe for their audience. Tests pin codes and stable identifiers rather than message wording. The [CLI spec](../specs/cli.md#each-refusal-has-a-stable-code) owns the contract.

## Consequences

A code is public surface. Message wording can improve independently of its cause.
