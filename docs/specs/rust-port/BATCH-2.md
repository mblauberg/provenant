# Rust port batch 2

Batch 1 merged one PR per crate (#1290 to #1307) plus rulebook versions 3 and
4. Nearly every PR then needed a cross-family review, a repair lane, a CI fix
and a merge from main. Batch 2 runs fewer, larger lanes: one lane per crate
taking every ready unit (`ready.py CRATE 12`), the self-check list in
[LANE-BRIEF.md](LANE-BRIEF.md), one cross-family review per PR, and small CI
breaks fixed by the chair directly.

## Ownership

Two chairs split the crates so that no two lanes write one crate.

| Chair | Crates |
|---|---|
| workshop-provenant | core, records, supervisor, confinement, config, run, routing, providers, quota, telemetry, cli |
| provenant (laptop) | workflow, fabric-runtime, fabric-store, fabric-mcp |

Each chair dispatches, reviews and lands its own lanes, and appends fully
ported ids to [ported.txt](ported.txt) through the lane PRs.

## Foundations

These two lanes unblock most partial units and start first.

1. **core Python compatibility.** Add `provenant-core::py` with the verified
   helpers now copied into crate `pending` modules: `repr`, `str`, `strip`,
   truthiness, JSON equality across int, float and bool, ISO timestamp parsing
   and the float-second `fromtimestamp` helper. Later crate lanes delete their
   copies. The checked integer codec stays in
   `provenant_records::checked_json`.
2. **Supervisor safe filesystem API.** The graph has no unit for it, yet the
   custody, storage, installation and lease units wait on it: descriptor-relative
   `openat` with `O_NOFOLLOW`, `mkdirat`, no-replace rename, `unlinkat`,
   directory fsync and bounded reads. The supervisor lane adds it alongside
   `workspace:front-door-syscalls`.

Crate lanes run in parallel with the foundations and use `pending` until the
shared owner merges. A final integration pass removes what is left.

## Decisions

- Integers outside i64/u64 raise a typed `NumberOutOfRange` (RP-records-005
  and its siblings). The user accepted this on 2026-10-23.
- RP-records-003: anchored tree removal refuses `..` components. See the
  divergence entry for the rationale.
