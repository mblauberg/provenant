# Disk use and garbage collection
Owner: provenant
Anchor: tools/front-door/src/tick.rs

What `provenant disk report` and `provenant gc` do and the decisions around
them; the rules are in the [disk spec](../specs/disk.md) and every key is in
`provenant config schema --all` (`retention.*`, `gc.*`, `gc.projects`).

## Inspect, preview, apply

```sh
provenant --project /absolute/path/to/repo disk report --json   # read-only; check data.errors first
provenant --project /absolute/path/to/repo gc --dry-run --json  # preview everything
provenant --project /absolute/path/to/repo gc --apply --json
```

- The report sizes every class (lane caches, attempt temp, provider state,
  captures, payload, worktrees) without changing state. It has a budget of
  50,000 entries and two seconds; a limit, permission or cross-device error makes
  totals partial. A lane entry with `reclaim: "gc"` names the owner but the
  report never runs it. `data.method: "allocated_fallback"` means allocated
  blocks, not APFS private bytes.
- Bare `gc` deletes eligible caches and previews merged-worktree, declared-output
  and ref cleanup; `--apply` deletes those too; the scheduled sweep applies.
  `--pressure` is the capped (five seconds) sweep writer admission runs when
  free space is below `gc.free_floor`; it fetches nothing and removes no
  worktrees. `gc.schedule = false` disables scheduled sweeps. Full and pressure
  sweeps share `<state>/gc/collection.lock`; a busy collector is skipped.
- Preview evidence never authorises deletion: apply takes a fresh census after
  acquiring writer custody. A census it cannot complete keeps the tree, and the
  error names the process and why. Read `data.provider_state`, `build_dirs`,
  `worktree_cleanup[]` and `unobserved_projects` in the JSON; default output is
  only counts and exceptions.
- Chair session start and saves warn when free space on the state, checkout or
  lane Cargo volume is below twice `gc.free_floor`.

## What is removed and when

- Provider state: Codex caches at settlement; credentials after 24 hours; after
  `retention.provider_state_days` (default 7) transcripts move to
  `provider-history/` and the rest of `provider-state/` is deleted, so resume
  returns `PROVIDER_STATE_EXPIRED` (start a fresh lane with the retained result).
  Results, exit evidence and digests stay. After `retention.logs_days` (30),
  captures over 64 KiB keep only their last 64 KiB tails.
- Payload: `retention.payload_cap_bytes` (default 16 GiB) caps bulk payload under
  `runs/`; the oldest eligible attempts are evicted whole down to 75% of the cap
  (`payload: "evicted"` in `status`). Never evicted: attempts under 24 hours
  old, and those holding unique private Git objects, abandoned, reserved or the
  source of a pending continuation. Do not rename a refused tree into a
  tombstone by hand.
- Lane build output: idle Cargo profile directories (all `.cargo-lock` files
  free, nothing written within `build_dir_grace_seconds`) go least recently
  used first until within `build_dir_max_bytes` and the free floor.
  `gc.lane_cargo_cache_max` (32 GiB) bounds total lane Cargo caches; live and
  foreign caches count towards it but are retained.
- Worktrees: merged-worktree cleanup fetches `origin`'s default branch; a lane
  is merged only if its tip is contained in it (a squash without a receipt does
  not count), and a failed fetch keeps everything (`upstream_error`). Declare
  per project, keyed by `provenant fabric identity`:

  ```toml
  [gc.projects."PROJECT_ID"]
  regenerable = ["node_modules", "target"]   # defaults to ["target"] with a root Cargo.toml
  keep = ["web/target"]
  foreign_worktrees = "keep"                 # "merged-clean" opts in to removing unknown-writer worktrees
  ```

  `.provenant/scratch` is always regenerable when ignored. Idle declared output
  can be reclaimed without a terminal lane after `--idle-days` (`gc.idle_days`;
  one day below the floor). Undeclared ignored files such as `.env` block
  explicit removal. `--pressure` never removes foreign worktrees.
- Refs: after a lane lands, its worktree, local branch, remote branch and tracking
  ref are removed, but only refs the ledger shows Provenant created, only when
  the tip is an ancestor of the freshly fetched target (or an integration
  receipt proves it), and only while the tip still matches the ledger. Unlanded
  or unknown refs are kept regardless of age. Each outcome is in
  `worktree_cleanup[].refs`; to restore one, recreate the branch at the recorded
  `head`.
- Chair log payloads and receipts past the replay horizon (full `gc` only), old
  terminal graph checkpoints (`gc.graph_checkpoint_days`; those with pending
  work, open asks or wakes stay) and managed releases (the active one, the two
  newest and any running; skipped if the process census is incomplete).
- Completion cleanup deferred at settlement (`cleanup_deferred`) is finished by
  the next applied full or pressure sweep; `land` does its own.

On macOS the tick agent runs every 60 seconds and collects when its daily
deadline is due; logs are in `<state>/tick/` (`gc-result.json`,
`seed-result.json`). Other installs schedule `provenant tick` themselves.

## Detached services and unsettled lanes

Provider executables do not inherit the guardian's writer, custody or port
leases. Ordinary detached services may keep running without holding those lane
descriptors. Unsandboxed services have no Seatbelt membership mark; an open
resource path alone never authorises killing them.

If cleanup reaches its stop deadline, warnings name an observable PID, short
command and writer-lock resource, or state that the holder is unknown. Close
an identified service through its normal controls, or wait for it to exit, then
run `provenant reconcile NAME/NODE#N`. Descriptor discovery is bounded and may
be incomplete. A remaining unresolved reservation is a custody fence: do not
unlink `writer.lock` or delete the worktree to bypass it. Guardian loss retains
that fence even when no process holds the kernel lock.
