# ADR 0035: Four hard boundaries, flexible everything else

**Status:** Accepted

Changed: 2026-10-30, #2284; 2026-11-04 (shims and provider guards); 2026-11-12 (unsandboxed trusted routes, #3246); 2026-11-14 (2.0.0 runs from a fresh state root, #3078).

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
versioned and tolerate extra fields.

Interfaces change without compatibility shims, before and after 1.0: no legacy
readers, aliases, fallback paths or dual formats. A breaking change ships with
a release note and a clear operator step, such as draining active work or
moving to a fresh state root
([install](../runbooks/install.md#from-1x-to-20)).

The format-1 reset moved off the 2.0.0 install path. Deleting 1.x state in
place was a one-way step on owner data, with a recovery story that depended on
every store being classified correctly. 2.0.0 instead runs from a fresh state
root, copies only the owner configuration out of the 1.x root and never writes
that root, so no 2.0 code reads 1.x-shaped state and rollback is reinstalling
1.x. The reset code stays for a later, owner-gated removal of the 1.x root.

Provider and launch guards outside the four boundaries warn and continue, or
are configurable defaults with a visible source. They do not refuse a lane.
Dropping an optional grant, skill or tool with a named warning is preferred
over refusing the launch.

`crates/run/assets/policy.toml` is the single executable policy table. Records
owns `Rule`, `Policy` and `Effect`; consumers call `policy.check(code, facts)`.
Each row names credentials, Git isolation, data integrity, authority or no
boundary, its default strength and its knob. No rule without a boundary defaults
to refuse. Host, project and dispatch overrides resolve in that order, and
`provenant config show` includes the effective policy with its source. Lane
preambles render these same rows. Process census is advisory for admission;
only rules with a real waiting action support Queue. The exclusive writer lease
and retained-work Git fences remain authoritative. Declared paths within a
worktree are advisory write scope; settlement and handoff report out-of-scope
paths. `policy.WRITE_SCOPE=refuse` explicitly selects strict scope.

## Consequences

New guards need an observed failure in one of the four classes. Reviews treat
a lane or launch refusal outside them, or any guard that blocks ordinary work,
as a defect to loosen, not a safety gain. See
[Simple first](../specs/rust-workspace.md#simple-first) and the
[supervisor spec](../specs/supervisor.md).

Overturn when: data loss, credential exposure or unauthorised change is traced
to a guard this decision loosened.
