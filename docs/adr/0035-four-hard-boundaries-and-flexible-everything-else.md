# ADR 0035: Four hard boundaries, flexible everything else

**Status:** Accepted

Changed: 2026-10-30, #2284; 2026-11-04 (shims and provider guards).

## Context

Guards added for unobserved risks cost more than they saved: tolerant-config,
land-cleanup and advisory-guard work (#2268, #2256, #2284) repeatedly removed
refusals that blocked ordinary work, while the genuine failures were data loss,
credential exposure and unauthorised change.

## Decision

Four boundaries are strict and fail closed:

1. **Lane Git:** a writer lane works in its private Git directory and cannot
   reach shared refs except through host-owned delivery.
2. **Credentials:** a networked writer lane receives the GitHub credential
   unless `lanes.publish=false`; credentials never cross hosts.
3. **Object integrity:** Git objects are digest-checked and fsck-verified on
   receipt, and results bind base and head.
4. **Authority gates:** landing, publishing, handoff acceptance and chair
   switching need the held authority; knobs for them refuse malformed values
   rather than recovering to permissive defaults.

Everything else is flexible. User configuration warns and keeps valid settings;
other edge cases get a note, or a typed refusal of that one operation
([ADR 0034](0034-refusals-carry-stable-typed-codes.md)). Persisted formats are
versioned and tolerate extra fields.

Interfaces change without compatibility shims, before and after 1.0: no legacy
readers, aliases, fallback paths or dual formats. A breaking change ships with
a release note, and with `provenant config migrate` or a drain before
upgrade ([install](../runbooks/install.md#bootstrap-and-upgrade)) when
persisted state changes.

Provider and launch guards outside the four boundaries warn and continue, or
are configurable defaults with a visible source. They do not refuse a lane.
Dropping an optional grant, skill or tool with a named warning is preferred
over refusing the launch.

## Consequences

New guards need an observed failure in one of the four classes. Reviews treat
a lane or launch refusal outside them, or any guard that blocks ordinary work,
as a defect to loosen, not a safety gain. See
[Simple first](../specs/rust-workspace.md#simple-first) and the
[supervisor spec](../specs/supervisor.md).
