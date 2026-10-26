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

The attempt's result, stdout, stderr, exit evidence, records and digests stay
in place. Resume then returns `PROVIDER_STATE_EXPIRED`; start a fresh lane
with the retained result or transcripts as context.

Inspect `data.provider_state.removed`, `kept` and `errors` in the GC JSON.
Dry runs report planned paths without deleting or archiving them or creating
lock files. GC skips held locks, reservations and pending continuations.
An unreadable reservation prevents deletion. Resolve custody through normal
reconciliation or chair recovery, then rerun GC. Filesystem errors produce a
failed result with their paths; completed removals remain reported.

Directory removal first renames to an identity-bearing tombstone. A later
sweep retries an interrupted deletion only for the authorised path and
matching device and inode. Do not manually rename a refused tree into a
tombstone.

On macOS, the installed default-state agent runs every six hours.
`[gc] schedule = false` disables scheduled sweeps; manual GC and settlement
cache pruning remain available. See
[configuration](configuration.md#lane-cache-collection) for agent installation
and logs. Other installations need an explicit scheduler invoking
`provenant gc`.
