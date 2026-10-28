# Collect caches and expired provider state
Owner: provenant
Anchor: tools/front-door/src/gc_agent.rs

Preview collection before inspecting the named removals:

```sh
provenant --project /absolute/path/to/repo gc --dry-run --json
provenant --project /absolute/path/to/repo gc --json
```

Lane build-cache selection uses `--idle-days` and `--free-floor`; see
[cache collection](install.md#build-a-staged-release).
Provider-state retention covers indexed terminal attempts in the resolved
host run store and its child stores. It does not depend on the selected
project's cache idle threshold or free-space floor.

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

Directory removal first renames to an identity-bearing tombstone. A later
sweep retries an interrupted deletion only for the authorised path and
matching device and inode. Do not manually rename a refused tree into a
tombstone.

On macOS, the installed default-state agent runs at load and daily.
`[gc] schedule = false` disables scheduled sweeps; manual GC and settlement
cache pruning remain available. See
[configuration](configuration.md#lane-cache-collection) for agent installation
and logs. Other installations need an explicit scheduler invoking
`provenant gc`.

The disk guard and writer admission use pressure collection without network
fetches or worktree removal. Admission caps its single attempt at five seconds
and rechecks every output volume once. Applied full and pressure sweeps share
`<state>/gc/collection.lock`; a busy collector is skipped, not queued.
Manual `gc --pressure` prints a notice when scheduling is disabled, the project
is transient or no eligible data is in scope. JSON exposes the reason as `notice`.
Lane settlement collects that lane's declared ignored output and Cargo cache,
including when its worktree is retained for delivery pickup. Successful
settlement, delivery integration and `land` also prune eligible worktrees
and their caches through the existing custody checks. These event passes share
the collection lock, apply no idle delay and leave foreign worktrees alone.
Kept files, live writers and reservations still prevent removal. A busy
collector skips settlement collection; the daily backstop retries eligible
data. Provider-cache and temporary cleanup at settlement remain scoped to
their owner. `gc.schedule = false` leaves settlement and landing cleanup enabled.

Declared ignored output in an idle linked worktree no longer needs a terminal
lane. It still needs free writer custody and no live lane. Undeclared ignored
files such as `.env` block explicit removal, whose error names up to five
paths. Configure `regenerable`, `keep` and the opt-in
`foreign_worktrees = "merged-clean"` policy as described in the
[configuration runbook](configuration.md#lane-cache-collection).
Applied foreign collection removes any temporary writer state it created,
after removal or refusal. Existing worktree state stays in place.
Temporary-state cleanup can wait up to five seconds for repository custody
after collection stops; a cleanup failure is reported as a refusal.
GC requires a primary checkout. Bare repositories and their linked checkouts
are refused as collection roots; scheduled scans report them as unobserved.

A preview uses existing project custody only. An unregistered selected checkout
or missing custody hierarchy returns an observation error without initialising
it. Scheduled previews list such roots in `unobserved_projects`.
