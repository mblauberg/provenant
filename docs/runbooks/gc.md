# Collect caches and expired provider state
Owner: provenant
Anchor: tools/front-door/src/tick.rs

Repeated census state and refusal details within one GC command use the same
observation evidence. Preview evidence never authorises deletion: apply takes
a fresh census after acquiring writer custody, and refreshes after any
required output preparation. Foreign collection still requires a complete
`Free` result. GC performs census work outside the repository lock. Native
processes may start without taking the advisory writer lock, so this check is
not atomic exclusion; existing registration, identity, cleanliness and
non-forcing Git removal fences remain in force.

Preview collection before inspecting the named removals:

```sh
provenant --project /absolute/path/to/repo gc --dry-run --json
provenant --project /absolute/path/to/repo gc --json
```

After a successful managed install, the installer retains the active release
and the two most recently installed releases. It also retains any release
whose executable is still running. If the host process census is incomplete,
release pruning is skipped so an unobserved process cannot lose its release.
Older managed release directories are removed from `<state>/releases/`.

Manual `provenant gc` also rebuilds pending graph markers and collects old
terminal graph checkpoints using `gc.graph_checkpoint_days`, the same policy
as the daily tick. Checkpoints with pending work, open asks or scheduled wakes
are retained. `gc --dry-run` leaves graph checkpoints untouched.

Lane build-cache selection uses `--idle-days` and `--free-floor`; see
[cache collection](install.md#build-a-staged-release).
Provider-state retention covers indexed terminal attempts in the resolved
host run store and its child stores. It does not depend on the selected
project's cache idle threshold or free-space floor.

Full GC also prunes chair log replay receipts beyond the replay horizon and
removes payloads exceeding the age, count or byte limits across all project
sessions, including abandoned sessions. The JSON report's
`chair_logs.expired` counts removed or eligible entries and `chair_logs.errors` names
failures. A dry run counts eligible entries without changing them. Pressure
collection does not scan chair logs.

Set the resume window in `<state>/config.toml`:

```toml
[retention]
provider_state_days = 7
```

The range is 1 to 3650 days. GC rereads this key on each invocation, so it
requires no reinstall. Ages start at the attempt's terminal `finished_at`.
The first sweep applies the rule to existing attempts.

At settlement, Codex's regenerable `cache/`, `plugins/`, `skills/` and
`models_cache.json` are removed. A continuation keeps its sessions and
rebuilds or fetches caches through the provider's normal startup.

On the first eligible sweep after 24 hours, GC removes known credential
entries such as `auth.json`, `.credentials.json` and the private keychain
link. It unlinks symlinks without touching the host credential they name.
After the configured window, GC archives transcripts in
`runs/<run>/attempts/<attempt>/provider-history/candidate-N/` and deletes the
remaining `provider-state/`. Codex's sessions, archived sessions, history
files, state and thread-history databases survive with their WAL, SHM and
journal companions. Claude's private `claude-config/projects/` survives.
Unclassified providers, including agy, keep their remaining private payload
as history until their transcript locations are established.

The attempt's result, exit evidence, records and digests stay in place.
After `retention.logs_days` (default 30), captures larger than 64 KiB from
closed runs expire, and intermediate event log rows are deleted from the
database. Their last 64 KiB stays in `stdout.log.tail` and
`stderr.log.tail`, preserving the distinction from the original capture
digests. Small captures stay unchanged. The rule also applies inside child
stores; child databases and result evidence remain. Inspect
`data.provider_state.attempt_logs` for planned or applied capture expiry. Resume then returns `PROVIDER_STATE_EXPIRED`; start a fresh lane
with the retained result or transcripts as context.

Inspect `data.provider_state.removed`, `kept` and `errors` in the GC JSON.
Temporary-directory byte counts are best-effort estimates. Incomplete
measurement is reported in `errors` without blocking temporary removal.
A temporary cleanup failure does not block credential expiry, provider-cache
pruning or provider-state expiry. Applied removal clears the attempt's
temporary-retained hint without rewriting its durable records or evidence.
Dry runs report planned paths without deleting or archiving them or creating
lock files. GC skips held locks, reservations and pending continuations.
An unreadable reservation prevents deletion. Resolve custody through normal
reconciliation or chair recovery, then rerun GC. Filesystem errors produce a
failed result with their paths; completed removals remain reported.

## Bound run payload bytes

`retention.payload_cap_bytes` (default 16 GiB; see
[configuration](configuration.md)) caps the bulk payload under `runs/`:
`provider-state/`, `provider-history/`, `scratch/` and capture bytes beyond the
64 KiB tails. When the accounted total exceeds the cap, GC evicts the oldest
eligible attempts first, whole attempts at a time, until the total is at or
below 75% of the cap.

- A payload whose attempt finished under 24 hours ago is never evicted.
- Provider state and logs older than `retention.provider_state_days` (seven by
  default) and `retention.logs_days` expire regardless of the cap, as above.
- An attempt that holds unique private Git objects (including after a refused
  import), is abandoned, reserved, unproven lost or the source of a pending
  continuation is pinned: neither age nor pressure evicts it. Eviction takes the
  attempt's publication and writer locks and rechecks custody under them.
- Records, results, exit evidence, receipts and capture tails stay. The attempt
  gains `payload-evicted.json` and `provider-state-expired.json`; `output` and
  `status` still work and report `payload: "evicted"`, and resume returns
  `PROVIDER_STATE_EXPIRED`.

Sizes come from `<state>/payload-ledger.json`, rewritten by full `gc` (the daily
tick). Settlement-time pressure collection only reads the ledger, so the
settlement path never walks payload trees. Inspect `data.provider_state.payload`
in the GC JSON: `accounted_bytes`, `pinned_bytes` (cannot be evicted),
`reclaimed_bytes` (this pass; planned in a dry run), `total_reclaimed_bytes`
(lifetime), `evicted` and `kept`. `provenant disk report` shows `payload`
with the same pinned and lifetime-reclaimed bytes.

Directory removal first renames to an identity-bearing tombstone. A later
sweep retries an interrupted deletion only for the authorised path and
matching device and inode. Do not manually rename a refused tree into a
tombstone.

On macOS, the installed default-state host tick runs every 60 seconds and
collects when its persisted daily deadline is due.
`[gc] schedule = false` disables scheduled sweeps; manual GC remains available. See
[host tick](#host-tick-and-low-disk-alerts) for logs and results. Other installations can schedule `provenant tick` explicitly.

Writer admission uses pressure collection without network fetches or worktree
removal, capped at five seconds. Applied full and pressure sweeps share
`<state>/gc/collection.lock`; a busy collector is skipped, not queued.
Manual `gc --pressure` prints a notice when the project is transient or no
eligible data is in scope; JSON exposes the reason as `notice`.

Fabric settlement records a retained receipt with `cleanup_deferred = true`, then
spawns one detached worker to collect the lane's worktree, declared output and
Cargo caches. Settlement fetches no refs and walks no payload trees. The worker retains
all ownership, continuation, terminal-state, live-writer, landing-proof and
expected-tip lease checks, and replaces the receipt with its outcome. Completion
cleanup keeps no queue. When the spawn fails, the worker dies or collection
custody stays busy for 120 seconds, the settlement receipt stays non-final
(`cleanup_deferred` or `retryable`) and nothing else is recorded. The next
applied full or pressure sweep, holding collection custody, runs the same
owner-bound collection for every terminal lane whose receipt is not final and
whose worktree still exists, landing against the fetched default branch (falling
back to the owner's base when the fetch fails, there is no origin, or the sweep
is a pressure sweep, the same rule settlement uses). That pass proves the owner
attempt, continuation writer and exact HEAD, so it collects a lane landed by
fast-forward that the path-based pass keeps at the integration tip or on its
first-parent line, and writes the `pruned` receipt.
`land` retains its explicit cleanup.
Kept files, live writers and reservations still prevent removal.

`gc --dry-run` uses the existing run-size ledger and shared bounded filesystem
samples. It does not refresh upstream refs or survey build profiles and target
seeds; `notice` identifies these deferred surveys. Incomplete cache sizes set
`data.bytes_estimated = true`, and an incomplete newest-write observation
cannot mark a cache idle. Applied GC obtains the full proof before deletion.

`gc.lane_cargo_cache_max` bounds the total lane Cargo cache at 32 GiB by default.
After orphan and idle collection, GC evicts remaining eligible caches oldest
first. It counts live and foreign caches towards the total while retaining them;
`data.total_bytes_after` may therefore exceed the cap. Existing worktrees need
writer exclusion before detachment; unknown custody retains the cache.

## Collect lane build output

Cargo profile directories in a lane cache (`target/debug`,
`target/<triple>/release`, or the same under `target-lint/`) are collected by
`tools/front-door/crates/run/src/build_dir_gc.rs#collect`. A directory is idle
when a non-blocking exclusive `flock` succeeds on all of its `.cargo-lock`,
`.cargo-build-lock` and `.cargo-artifact-lock`. Cargo holds them while it builds,
so a directory with any held, or that cannot be probed, is never removed.
Idle directories go least recently used first, by the newest modification time
below each, until the allocated bytes of all profile directories, idle and busy
alike, are within `build_dir_max_bytes` and free space on the cache volume is at
least `gc.free_floor`. When busy directories alone exceed the cap, every
eligible idle directory goes and the total stays over. Space outside profile
directories (`target/doc`, Cargo homes, other lane files) is not counted or
collected here.

Free locks do not prove a directory is unused: test and run binaries keep
running after Cargo drops its locks. A directory whose newest modification is
within `build_dir_grace_seconds` is never removed, nor is one written after the
sweep measured it; it still counts towards the cap and is reported as `recent`.
A modification time more than five minutes in the future is clock skew and earns
no grace. Zero turns the grace off. A test or run binary that runs longer than
the grace without writing under `target/` is unprotected once its Cargo locks
are free. Defaults and ranges are in
`provenant config schema --all`.

Collection runs on two events:

- an applied `provenant gc`, including the scheduled sweep and `--pressure`;
- lane admission that finds lane disk space below `gc.free_floor`.

`gc --dry-run` does not survey build output. The walk takes no lock, and a sweep
that finds the cap and floor already met stops there. Only removal holds the host
admission lock, so no lane is admitted mid-removal; it probes each candidate
again under that lock. Admission walks only when free space is below the floor,
when the last walk is older than `build_dir_survey_interval_seconds`, or when
that walk found the total within 10% of the cap. The result is kept in the state
root and an admission that finds another walk running skips. Settlement does
not walk lane caches.

`provenant gc` reports the directories removed, the bytes reclaimed, how many
bytes of build output remain and how many directories were busy or recent. The
JSON form has `build_dirs` with `removed`, `bytes`, `cache_bytes`, `busy`,
`recent` and `errors`; the summary form has `build_dirs_removed`,
`build_dir_bytes_reclaimed` and `build_dir_bytes`. A failed collection is
reported as an alert and the next event retries.

## Choose worktrees and output to collect

Bare `provenant gc` deletes eligible caches and previews merged-worktree,
declared-output and ref cleanup. `--apply` deletes those too; `--dry-run`
previews everything. The scheduled sweep applies. Merged-worktree cleanup
fetches `origin`'s actual default branch, so it needs no fresh local `main`; a
failed fetch keeps worktrees and reports `upstream_error`. A worktree needs a
terminal lane and commits of its own: a tip equal to upstream or on its
first-parent history stays. Containment in the fetched default proves a merge,
including a lane merged through a batch branch. A locally merged batch, or
squash-only equivalence, does not.

`gc.projects` is keyed by the registered Fabric project ID (see
`provenant fabric identity`):

- `regenerable` lists directory names matched at any depth in the project's
  registered linked worktrees. With no project entry, a root `Cargo.toml`
  defaults it to `["target"]`; other projects default to empty. An explicit
  entry overrides the default.
- Provenant's own lane scratch, `.provenant/scratch`, is always regenerable
  when Git reports it ignored, in addition to `regenerable`. Removing a worktree
  whose only ignored output is that scratch needs no declaration.
- `keep` lists worktree-relative paths. They, their descendants and any matching
  directory containing them are preserved, as are tracked paths. `.git`,
  `.worktrees`, absolute names and parent traversal are refused. Declared output
  must be Git-ignored.
- An idle registered linked worktree can have declared output reclaimed without a
  terminal lane when no lane or writer lease is live. The minimum idle age is
  `--idle-days` (scheduled GC uses `gc.idle_days`), or one day when the state
  volume is below the floor. The primary checkout stays protected. JSON reports
  this class as `worktree_cleanup[].idle_output`. A failure skips that entry.
- `foreign_worktrees = "merged-clean"` opts in to removing linked worktrees that
  otherwise report `unknown_writer`. It requires fetched-default ancestry, a
  clean tree once only declared ignored output is removed, no live lanes or
  reservations, a free existing writer lock where present and a complete
  free-process census; a missing process observation refuses removal. Git
  registration and filesystem identity are verified again under repository
  custody. It also covers Git-registered worktrees outside `.worktrees/`, and
  deletes an unchanged branch not checked out elsewhere. The default `"keep"`
  retains the terminal-lane requirement. `--pressure` never removes foreign
  worktrees.

Scheduled GC excludes missing Git roots, non-Git workspaces and known staging,
probe, run and scratch roots before it collects project IDs. Old identity
records stay available for indexed run evidence. `unobserved_projects` in the
JSON is an array of paths whose custody could not be observed; `skipped_transient`
lists excluded transient common directories.

## Host tick and low-disk alerts

The macOS tick agent writes `<state>/tick/stdout.log` and
`<state>/tick/stderr.log`. Collection runs in a detached one-shot worker whose
latest result is `<state>/tick/gc-result.json`, so a slow sweep does not delay
graph wakes. It discovers registered projects without a checkout as its working
directory, and unobservable projects keep their caches. When collection
finishes, a second detached worker refreshes each project's Cargo target seed
from the freshly fetched `origin/main`; its result is
`<state>/tick/seed-result.json`. See [worktrees](../worktrees.md).

After collection, GC compares free space on the state volume with
`max(gc.alert_below, gc.free_floor)`. An applied sweep records a Fabric note
with entity `gc.low_disk` once per dip and sets a latch at
`<state>/gc/low-disk.json`; a reading at or above the threshold clears it. A
preview reports low space without touching the latch. An alert failure is
reported without blocking collection. Both keys are reread on each run.

## Remove finished lanes' refs

Provenant removes what a finished lane leaves behind once its work has
landed: the worktree, the local branch, the remote branch and its
remote-tracking ref. It needs no action from you.

- Only refs the ref ledger records as created by Provenant are removed. A
  branch it found already there (adopted), or one it never observed, stays.
  The ledger lives at `<state>/worktrees/<project>/refs/`.
- A ref has landed when the shared landed check proves it: its tip is an
  ancestor of the freshly fetched target, or an integration receipt chain
  carries it there. Tree or patch-id equality is never proof, so a squash
  merge without a receipt is not landed.
- Unlanded refs are reported and retained regardless of age. An unknown
  landing state also retains the ref. A tip that differs from the ledger's
  recorded tip is retained, including commits added by a human.
- Every deletion is leased on the tip just checked: a local ref is deleted
  with `update-ref -d NAME TIP` and a remote branch with
  `git push --force-with-lease=REF:TIP URL :REF`. A ref that moved is kept.
- A branch any worktree has checked out, the integration branch itself,
  dirty worktrees, worktrees with a live writer and declared `keep` paths
  are retained.
- Remote names and push URLs come from the delivery endpoint recorded with
  each ref, never from a default remote name.

In completion GC, `prune_owned` removes the lane's worktree and, for an
integrated lane, its created local branch; an integrated lane's own created
remote branch goes when the integration receipt proves it landed. A pushed
lane is not landed at settlement; its refs go on the first full `gc` pass
after the target contains them. `gc` lists every created ref in the ledger,
fetches each bound remote's default branch once, and removes only proven
landed refs whose tips still match the ledger. `gc --pressure` skips this network step.

Each artefact reports one outcome under `artefacts` in the settlement receipt
and `worktree_cleanup[].refs` in `gc --json`:

```json
{"artefact":"remote_branch","name":"refs/heads/lane/x","remote":"origin",
 "outcome":"removed","reason":"landed","head":"0123..."}
```

`artefact` is `worktree`, `local_branch`, `remote_branch` or `tracking_ref`.
`outcome` is `removed`, `would_remove` (preview), `absent`, `retained` or
`failed`; `reason` says why, for example `landed`, `not_landed`, `not_created`, `checked_out`, `changed` or
`target_unavailable`. A failed removal prints a warning and never fails the
settlement or the pass; the ref is retained and the next pass retries. To
restore a removed ref, recreate it at `head`, for example
`git branch lane/x HEAD_OID` or `git push URL HEAD_OID:refs/heads/lane/x`.

Declared ignored output in an idle linked worktree no longer needs a terminal
lane. It still needs free writer custody and no live lane. Undeclared ignored
files such as `.env` block explicit removal, whose error names up to five
paths. Configure `regenerable`, `keep` and the opt-in
`foreign_worktrees = "merged-clean"` policy as described in the
[configuration runbook](configuration.md#lane-cache-collection).
On a full pass, fetched-default ancestry permits immediate removal of a
clean merged tree and its unchanged local branch and own cache; recent
declared ignored output does not delay that removal. Standalone output and
cache collection still use their configured age thresholds.
Applied foreign collection removes any temporary writer state it created,
after removal or refusal. Existing worktree state stays in place.
Temporary-state cleanup can wait up to five seconds for repository custody
after collection stops; a cleanup failure is reported as a refusal.
GC requires a primary checkout. Bare repositories and their linked checkouts
are refused as collection roots; scheduled scans report them as unobserved.

A preview uses existing project custody only. An unregistered selected checkout
or missing custody hierarchy returns an observation error without initialising
it. Scheduled previews list such roots in `unobserved_projects`.

Default GC output shows aggregate counts and exceptions. Use `--json` for the
full removal and retention records. Completion GC runs promptly after settlement; the daily sweep also retries
eligible data left by a busy or interrupted pass.
