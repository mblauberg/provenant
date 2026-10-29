# ADR 0034: Refusals carry stable typed codes

**Status:** Accepted

Changed: 2026-10-30, #2273.

## Context

Callers and tests matched refusals by category or message prose, so rewording a
message or reusing `REFUSED` and `IO` for unrelated causes broke agents and
hid which remedy applied.

## Decision

Every refusal carries its own stable `error.code` that names the cause, over
and above the category that drives status and exit code. Each owner declares
its codes once, with meaning, in a typed enum (for run, `ErrorKind`); the CLI
lists them in `help --all --json`, and the generated reference renders that
list. Typed causes survive across crate, graph checkpoint and MCP boundaries
instead of being flattened to prose. Tests assert the code and stable
identifiers, never message text.

## Consequences

A code is public surface: renaming one is a breaking change, so add rather than
reuse. Message wording is free to improve. Remaining coarse codes are converted
as owners are touched. See the [CLI spec](../specs/cli.md#each-refusal-has-a-stable-code).
