## Behaviour changes

- Admission now gives each lane a class: `writer` for a `worktree-write` lane,
  `read_only` for every other. Startup spacing is measured per class. Writers
  keep the 20 s gap after the last writer start. Read-only lanes wait 2 s after
  the last read-only start and no longer queue behind writers, so eight
  read-only lanes start in about 14 s instead of 2 min 20 s. The memory (5 %)
  and disk (500 MiB) floors still refuse both classes.
- The per-project active lane cap is 32, up from 16. The host cap stays at 64.
- A batch with no `worktree-write` lane now defaults to 16 lanes at once, from
  the new `lanes.max_concurrency_read_only` knob (1 to 16, default 16; the
  `economy` preset sets 8). Batches with a writer keep the `lanes.max_concurrency`
  default of 8. An explicit `--concurrency` or Fabric `concurrency` still wins,
  and its upper bound is now 16.
- The host admission ledger (`admission.sqlite3`) moves to schema version 2 and
  upgrades in place on first use. Its single spacing row becomes the writer
  start, and existing leases and tickets with no class count as writers. A
  binary from before this release refuses the upgraded ledger, so upgrade every
  host together.
