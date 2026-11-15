# ADR 0035: Four hard boundaries, flexible everything else

**Status:** Accepted

Changed: 2026-11-15, #3078

## Context

Guards added for unobserved risks cost more than they saved: tolerant-config,
land-cleanup and advisory-guard work (#2268, #2256, #2284) repeatedly removed
refusals that blocked ordinary work, while the genuine failures were data loss,
credential exposure and unauthorised change.

## Decision

Four boundaries are strict and fail closed:

1. **Lane Git:** a writer lane works in its private Git directory and cannot
   reach shared refs except through host-owned delivery. A sandboxed lane is
   kernel-denied shared Git writes; an unsandboxed lane on a trusted route is
   held by delivery checks and route trust ([ADR 0040](0040-open-lane-policy-with-explicit-boundaries.md#sandbox-mode)).
2. **Credentials:** credentials never cross hosts. GitHub access and its
   read-only credential-store exception are governed by [ADR 0040](0040-open-lane-policy-with-explicit-boundaries.md#permitted-exceptions).
3. **Object integrity:** Git objects are digest-checked and fsck-verified on
   receipt, and results bind base and head.
4. **Authority gates:** landing, publishing, handoff acceptance and chair
   switching need the held authority; knobs for them refuse malformed values
   rather than recovering to permissive defaults.

Everything else is flexible. User configuration warns and keeps valid settings;
other edge cases get a note, or a typed refusal of that one operation
([ADR 0034](0034-refusals-carry-stable-typed-codes.md)). Persisted formats are
versioned, and each owner defines its current field shape.

Interfaces and stored shapes change without compatibility shims, older-shape
readers, aliases or general migrations. The sole bounded exception is
additive SQLite schema steps within one state-root format major. A store's
`user_version` is a per-store base identifier fixed for that root major; it
does not advance independently. A non-additive stored change requires the
next root format major and a fresh state root.

The 1.x to 2.0 handoff does not migrate 1.x state or read its install receipt.
It copies owner configuration only. Install removes or overwrites its declared
Provenant artifact names; anything else left by 1.x is an operator's manual
release step or a doctor warning. Breaking changes include a clear operator
step when one is needed.

Provider and launch guards outside the four boundaries warn and continue, or are configurable defaults with a visible source. The policy is declared once and overrides remain visible.

## Consequences

Hard holds require a real boundary in one of the four classes. Reviews treat
a lane or launch refusal outside them, or any guard that blocks ordinary work,
as a defect to loosen, not a safety gain. See
[Build for change](../specs/rust-workspace.md#build-for-change-refuse-what-can-lose-data) and the
[supervisor spec](../specs/supervisor.md).

Overturn when: data loss, credential exposure or unauthorised change is traced
to a guard this decision loosened.
