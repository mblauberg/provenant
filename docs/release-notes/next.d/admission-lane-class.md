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
  default of 8. A `lanes.max_concurrency` you set yourself also caps the
  read-only default unless you set the read-only knob too. An explicit
  `--concurrency` or Fabric `concurrency` still wins, and its upper bound is now
  16. A value above the batch's task count is accepted by the CLI, a local
  Fabric call and a peer alike; before, a peer refused it while the CLI did not.
- `lanes` now shows `lane_class` and `waiting_for` for queued and starting
  lanes, and uses the wait as their reason: launch not yet handed to a
  guardian, run concurrency, the class's remaining startup spacing, or host
  capacity. A peer's lanes, from `lanes --host` or a placed run, show the same
  fields; an older peer shows neither.
- The host admission ledger (`admission.sqlite3`) moves to schema version 2 and
  upgrades in place on first use. Existing leases and tickets with no class
  count as writers, and any old wall-time start, whatever its age, is carried
  across for each class as a start at the upgrade, so the one-time upgrade holds
  each class for one spacing. The wall clock is not read. The upgrade is refused if the host's clock or boot
  identity cannot be read. A binary from before this release refuses the upgraded ledger,
  so upgrade every host together.
- Startup spacing no longer depends on the wall clock. It is measured on the
  host's monotonic clock since boot, which every process on the host shares and
  no clock change can move, and each class's last start is kept with the boot
  it was read in. Within one boot, no two lanes of a class start less than one
  spacing apart, whatever the wall clock does, including a step back then
  forward or a step forward alone. A reboot resets spacing, so the first lane
  of each class after a boot starts at once (the one-time version 1 upgrade
  above holds each class for one spacing instead). If the host's clock or boot
  identity cannot be read, the lane's start is refused with `startup spacing
  cannot be judged: host clock or boot identity unreadable` instead of falling
  back to the wall clock. Spacing is enforced at admission, not at provider
  spawn, so a guardian suspended between the two can spawn alongside a later
  lane.
