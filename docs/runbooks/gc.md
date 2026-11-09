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
[configuration](configuration.md#lane-cache-collection) for tick installation
and logs. Other installations can schedule `provenant tick` explicitly.

Writer admission uses pressure collection without network fetches or worktree
removal, capped at five seconds. Applied full and pressure sweeps share
`<state>/gc/collection.lock`; a busy collector is skipped, not queued.
Manual `gc --pressure` prints a notice when the project is transient or no
eligible data is in scope; JSON exposes the reason as `notice`.

Fabric settlement records `cleanup_deferred = true` and immediately queues a
detached completion GC worker to collect its worktree, declared output and Cargo
caches. Settlement fetches no refs and walks no payload trees. The worker retains
all ownership, continuation, terminal-state, live-writer, landing-proof and
expected-tip lease checks. Durable requests under `<state>/gc/completions/`
survive interruption; each host tick retries them independently of the daily
GC deadline and `gc.schedule`. A busy worker waits up to 120 seconds for collection
custody before leaving its request for retry. `land` retains its explicit cleanup.
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
