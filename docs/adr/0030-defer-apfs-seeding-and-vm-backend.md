# ADR 0030 - Defer APFS clone seeding and a VM sandbox backend

**Status:** Proposed 2026-10-25 (trials from issue 1452; no code changed).

## Context

Issue 1452 asked for two measured trials: seeding a new worktree's Cargo target
with `cp -c` (APFS clonefile) from a warm donor, and an opt-in VM backend for
Linux-shaped work with Seatbelt staying the default.

## APFS clone seeding: not adopted

Debug build of `provenant` (`cargo build --locked --bin provenant`,
`CARGO_BUILD_JOBS=4`), macOS 27.0.1, serial. Seeded runs cloned a donor
`debug/` built in another worktree path, as a real new worktree would.

| Run | Clone | Build | Total | Disk used after |
|---|---|---|---|---|
| Cold, first build (registry fetch included) | n/a | 76s | 76s | 1.4 GB |
| Cold, second worktree path | n/a | 40s | 40s | 1.4 GB |
| Seeded 1 | 2s | 32s | 34s | 1.46 GB |
| Seeded 2 | 2s | 28s | 30s | 1.30 GB |
| Seeded 3 | 2s | 28s | 30s | 1.69 GB |

The clone alone costs about 4 MB of new blocks. The build then rewrites
workspace crates and `incremental/`, so each seeded target ends near the size
of a cold one: sharing is lost as soon as cargo writes. Against a warm cold
build the saving is 10s (25%); the 76s figure is a one-off. That is below the
50% bar, so nothing was implemented. Revisit if a heavier dependency tree makes
cold builds dominate; the seam would be where the writer worktree's
per-worktree Cargo target is created (see `prune_orphaned_worktree_cargo_caches`
in `crates/confinement/src/permissions.rs`).

## VM backend: deferred, no build

Installed here: Docker CLI 29.8.2 (Docker Desktop context). Apple `container`
is not installed. The `docker sandbox` subcommand is removed from this CLI and
points at the separate Docker Sandboxes product, which is also not installed.

An opt-in backend would need: a launcher that mounts only the writer worktree
and the Fabric socket directory; per-provider credential and network policy
inside the guest (today Seatbelt profiles express this); a Linux build of the
front door and provider CLIs; and tests that the capability-compiled authority
(ADR 0002 lineage, ADR 0020) maps to guest mounts without widening.

**Recommendation:** keep Seatbelt as the only backend. Revisit when a task
genuinely needs Linux (a Linux-only test or toolchain) and the user wants it;
then prototype Apple `container` (no daemon, per-container VM, macOS 26+) behind
an explicit flag, ahead of Docker Sandboxes, which adds a third-party product
and licence surface.
