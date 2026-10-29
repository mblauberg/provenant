# ADR 0035: Four hard boundaries, flexible everything else

**Status:** Accepted

Changed: 2026-10-30, #2284.

## Context

Guards added for unobserved risks cost more than they saved: tolerant-config,
land-cleanup and advisory-guard work (#2268, #2256, #2284) repeatedly removed
refusals that blocked ordinary work, while the genuine failures were data loss,
credential exposure and unauthorised change.

## Decision

Four boundaries are strict and fail closed:

1. **Lane Git:** a writer lane works in its private Git directory and cannot
   reach shared refs except through host-owned delivery.
2. **Credentials:** a lane receives the GitHub credential only when
   `lanes.publish=true`; credentials never cross hosts.
3. **Object integrity:** Git objects are digest-checked and fsck-verified on
   receipt, and results bind base and head.
4. **Authority gates:** landing, publishing, handoff acceptance and chair
   switching need the held authority; knobs for them refuse malformed values
   rather than recovering to permissive defaults.

Everything else is flexible. User configuration warns and keeps valid settings;
other edge cases get a typed refusal ([ADR 0034](0034-refusals-carry-stable-typed-codes.md))
or a note. Persisted formats are versioned and tolerate extra fields. Pre-release
interfaces change without compatibility shims; the project makes no stability
promise before then.

## Consequences

Reviews block only on these boundaries, data loss or a broken main path. New
guards need an observed failure in one of the four classes. See
[Simple first](../specs/rust-workspace.md#simple-first) and the
[supervisor spec](../specs/supervisor.md).
