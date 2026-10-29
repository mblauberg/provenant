# Model routing
Owner: provenant-routing
Anchor: tools/front-door/crates/routing/src

Run `provenant route list` to inspect the catalogue, configured pool weights
and pacing policy; pacing and readiness apply at dispatch. Use
`provenant --json route list` for records, `--adapter NAME` to narrow it, or
`--pool NAME` for one pool. Resolve a pool with
`provenant route resolve --route NAME`, or select an exact model with
`--model ADAPTER/ID` and optional `--effort LEVEL`.
`provenant route resolve NAME` also accepts a route name or an unambiguous
model name. Unknown or ambiguous names report suggestions and examples.

`provenant route list --valid` lists catalogued model/effort pairs accepted by
resolution and the configured Go allowance floor. It includes an unspecified
effort row where accepted. Combine it with `--adapter` or `--pool`. It does not
probe executables, credentials, quota or live availability.

See [model policy](model-policy.md) for compaction, cache economics, account
overrides, read-heavy selection, result checks and cache-read share targets.

The resolver draws among ready positive-weight members using weight multiplied
by the effective pacing factor. `pace` uses the selected account's latest fresh
quota snapshot and takes the lowest factor across windows with a future reset.
Each factor divides remaining quota headroom by remaining window time and is
clamped to 0.25–2.0. A five-hour window at 85% used or a seven-day window at
90% used makes the account unavailable with `pacing_reserve`. Override the
default 15% and 10% reserves per adapter in `config.toml`, for example:

```toml
[domains.routing.pacing_reserve.claude]
five_hour_percent = 15
seven_day_percent = 10
```

Reserve percentages range from 0 to 100. Account profiles can also set a
reserve per selected window, including monthly windows. Expired resets do not
affect pacing unless an
active dashboard calibration still binds that window. Calibrated windows with
no current reset use a full remaining window for conservative pacing; see
[Go calibration](configuration.md#opencode-go-quota-calibration). Go pool
members remain eligible without floors, using the local ledger as a lower
bound. Route diagnostics identify uncalibrated windows. Missing quota data uses
1.0. `spend` and `max` use 1.0; admission handles
provider quota refusals, and a paid route remains eligible while credits
remain. If no weighted member is ready, the first ready zero-weight member is
selected; otherwise a recoverable unavailable member is preferred, and
disabled members are excluded. If only disabled members remain, resolution
returns their exclusions. The receipt records candidate readiness and
exclusions. Operation ID and task index seed selection so retries repeat the
same choice. Only OpenCode `*-free` entries are marked free and they require
`--allow-free` (default false). Agy Gemini Flash is a paid subscription model.
Omit `--effort` on OpenCode free routes: their recorded model controls do not
support effort, so an explicit value is refused before launch.
`--confidential` excludes training or unclassified members and fallbacks.
There is no classifier or route-synonym inference. Effort is set only when the
catalogue contains an exact recorded control; unsupported effort is refused.
`route resolve MODEL` reports the recorded supported efforts, including when a
model takes none. Uncatalogued exact IDs remain available as unverified models;
resolution and dispatch suggest nearby catalogue IDs, and an explicit effort
is refused because no controls are recorded for the unlisted ID.

The bundled Rust catalogue (`tools/front-door/crates/routing/assets/catalogue.toml`)
owns default pool membership, weights, pacing and effort controls. Override a
member with `provenant route set POOL MODEL [--weight N] [--effort E]
[--efforts a,b,c]`, remove one with `provenant route remove POOL MODEL`, or
restore a bundled pool with `provenant route reset POOL`. Set provider pacing
with `provenant route pacing PROVIDER pace|spend|max|off`. These edits are stored
under `[domains.routing]` in the selected state's `config.toml`. Catalogue
presence does not establish adapter availability; readiness is decided at
dispatch, so inspect the resolved route recorded for the attempt.

## Review shapes

Review shapes describe groups of read-only reviewers. Each group names a
routing pool and a count, and may assign one lens to each reviewer and request
distinct model families. Configure them in the selected state's `config.toml`:

```toml
[domains.routing.review.shapes.quick]
reviewers = [{ pool = "junior", count = 1 }]

[domains.routing.review.shapes.standard]
reviewers = [{ pool = "junior", count = 2, distinct_families = true }]

[domains.routing.review.shapes.deep]
reviewers = [
  { pool = "junior", count = 4, lenses = ["correctness", "security", "failure-isolation", "tests"], distinct_families = true },
  { pool = "senior", count = 2, distinct_families = true },
]

[domains.routing.review.select]
default = "standard"
by_risk = { low = "quick", high = "deep" }
by_paths = { ".github/workflows/**" = "deep" }
```

The bundled shapes are `quick`, `standard` and `deep`. Tables layer over the
bundled configuration; a `reviewers` array replaces the earlier array. Each
count must be between 1 and 32, and a non-empty `lenses` array must have one
identifier per reviewer. Pools and selected shape names must exist. A shape may
dispatch at most 64 reviewers.

For example, review a commit with changed paths from a Git range:

```sh
provenant review HEAD --brief-file review-brief.md --risk high --kind code --range origin/main...HEAD
provenant --json review HEAD --brief-file review-brief.md --shape quick
```

The target must resolve to a commit. `--range` supplies changed paths for
selection; without it no path rules match. Renames are treated as a deletion
and an addition, so rules can match either path. `--shape` overrides selection
and skips the range lookup. A changed path that is not UTF-8 is rejected; use
an explicit shape when path selection cannot represent it.
`--wait` sets the overall wait limit, defaulting to one hour, and accepts
seconds or durations such as `30m`, from one second to four hours.

Selection uses an explicit shape first, then the most specific matching `by_paths`
pattern, then `by_risk`, `by_kind` and finally `default`. Specificity is the
number of literal bytes, followed by fewer wildcards; lexical order breaks ties.
In path patterns, `*` and `?` do not cross directory separators; `**` does.
The selected shape and its source appear in the review summary. Family
distinctness is requested within each group; the summary records a substitution
when the pool cannot provide another family. Independent reviewers are submitted
without waiting for one another; a distinct-family group waits for each earlier
member's route before submitting its next member. Each reviewer goes through
normal dispatch, so pacing and quota apply per reviewer.

The summary collates each reviewer's run, route, state, verdict and full output
path without merging findings. A reviewer is asked to begin with
`Verdict: approve`, `Verdict: request_changes` or `Verdict: inconclusive`.
Leading blank lines, case and Markdown emphasis are accepted; an unrecognised
or unreadable result is `unknown`. JSON includes verdict counts, with planned
but `not_submitted` reviewers counted separately. The command
exits successfully only when every reviewer succeeds and approves; a failed,
inconclusive, change-requesting or unknown verdict is non-zero. This result
does not itself decide delivery acceptance.

Retries use deterministic reviewer operation IDs derived from the resolved
commit, selected shape and its configuration, brief and reviewer position.
Run the same review again to reattach to running or successful runs, resubmit
failed or cancelled runs, and submit any remaining reviewers. Keep the commit,
shape configuration, brief and actor identity unchanged. The actor depends on
`PROVENANT_AGENT_LABEL` or the configured agent label; using a different label
starts a separate review. A changed `HEAD`, shape or brief also starts a
different review. A successful run with an unknown verdict is reattached, so
retrying alone does not replace its output. On timeout or error, the
summary lists all planned reviewers, including `not_submitted` entries, and
prints commands to collect or cancel active runs. Active runs are not cancelled
automatically. Use `provenant lanes --id RUN --wait 4m` to collect one, or
`provenant cancel RUN` to cancel it.

## Per-account pacing

Account profiles layer over provider pacing. An active temporary override takes
precedence, followed by the account profile, provider default and bundled
default. This holds for free models too: the selected account's profile (and
any override) governs them, so an account set to `off` cannot serve a free
model. The `pacing.free.<provider>` profile is the provider default for free
models, used when the account has no profile or no policy. A profile can choose `spend`, `pace`, `max` or `off`; `off` prevents
routing. For `pace`, choose the windows to include, or omit `windows` to use
all known windows. The lowest factor governs. Missing or unobservable windows
are neutral and can begin to affect pacing when they become observable.
Windows without a reset also remain neutral. Profile names map to quota
snapshot windows as follows:

| Profile window | Snapshot match |
|---|---|
| `five_hour` | A duration of 18,000 seconds, `five_hour`, or a pool ending in `.5h`. Claude and OpenCode use `five_hour`; Codex `primary` and Agy pools such as `gemini.5h` are recognised by duration. |
| `seven_day` | A duration of 604,800 seconds, `seven_day`, or a weekly pool. This includes Claude and OpenCode `seven_day`, Codex `secondary`, and Agy pools such as `claude.weekly`. |
| `monthly` | `monthly`, `thirty_day` (OpenCode), or `billing_cycle` (Cursor). |
| `rolling` | OpenCode's `five_hour` or `rolling` pool. Its five-hour ledger is a rolling window. |
| `daily` | A duration of 86,400 seconds or a `daily` pool. No provider reports one yet, so it stays neutral until a daily window is observable. |

Numeric duplicate-name suffixes such as `:2` are ignored when matching names.
Names and durations come from the provider decoders in
`tools/front-door/crates/providers/src/quota/mod.rs` and the OpenCode ledger in
`quota_live/opencode.rs`. OpenCode's rolling five-hour window currently has no
reset, so it remains neutral until a reset is observable. Its weekly and
monthly windows have resets when cycle anchors are configured. These ledger
estimates may differ from the dashboard; calibrating them is separate work.

Omit `deadline` to pace to each window's own reset. A deadline can instead be:

- `{ reset_of = "anese" }`, optionally with `window = "seven_day"`, to use
  another account's observed reset. The default reference window is seven-day.
  If the reference reset is unknown or stale, pacing falls back to the paced
  window's own reset and `route resolve` reports the fallback. The reference
  account's newest fresh snapshot is authoritative: if it reports an unknown
  status, an older snapshot's reset is not used and the report names the status.
- `"thu 00:00"` or `"day 25"` for a weekly or monthly point in the host's local
  time zone. A monthly day beyond a shorter month's end, such as `"day 31"` in
  February, falls on that month's last day, and `route resolve` states the
  clamping.
- `"2026-11-02T09:00"` for a one-off absolute time in the host's local time zone.

A reset reference stays within the paced window's current cycle: when the
peer's freshly observed reset advances beyond the own reset, its previous reset
marks the handover, if it lies within the current cycle. This keeps a standby
account on standby until its own reset.

Pacing targets the earlier of the deadline and the window's own reset. After
that deadline, `standby` (the default) gives the account weight zero while
another candidate in the pool can serve; `continue` instead paces to the reset.
Removing `deadline` switches an account back to its own resets. Policies,
window selectors and deadline forms are validated together. Unknown configuration
keys warn and are ignored; invalid optional policy rows use defaults or are
omitted while valid rows remain active. Model policy recovers per field, so an
invalid `max_continuations` leaves a sibling `read_only` in force. Invalid
enforced fields (`read_only`, `max_context`, `max_turns`), or a malformed rule
that sets one, refuse the configuration instead.

These profiles illustrate how different accounts can share a provider:

| Account | Intended pacing |
|---|---|
| codex | Spend freely, using free credits and a banked reset. |
| pro (Claude) | Pace the five-hour window as the spare account. |
| michael (Claude) | Pace weekly usage to anese's reset, around Thursday, then stand by. |
| anese (Claude) | Pace weekly usage from around Thursday to michael's reset, around Sunday. |
| cursor | Pace to the monthly allowance. |
| opencode go | Pace to the monthly allowance while respecting weekly and rolling windows. |
| opencode free | Use whenever available; a daily window can be added when observable. |
| agy | Set host-specific deadlines to stagger accounts, or pace both to their weekly limits. Gemini and Claude pools apply on both. |

Account profile keys and `reset_of` accept registered account keys, entry IDs,
or labels unique across adapters. Register the accounts before using the example;
unknown or ambiguous names are reported with the available labels and keys.
Use `adapter/key` to disambiguate a key shared by providers. Do not configure
multiple profiles for the same account under different names. Remove or update
stale profile and `reset_of` references when unregistering accounts: an unknown
or ambiguous reference prevents route resolution and dispatch for all providers.

For example, in the selected state's `config.toml`:

```toml
[domains.routing.pacing.accounts.codex]
policy = "spend"

[domains.routing.pacing.accounts.pro]
policy = "pace"
windows = ["five_hour"]
reserve = { five_hour = 15 }

[domains.routing.pacing.accounts.michael]
policy = "pace"
windows = ["seven_day"]
deadline = { reset_of = "anese" }
after_deadline = "standby"

[domains.routing.pacing.accounts.anese]
policy = "pace"
windows = ["seven_day"]
deadline = { reset_of = "michael" }
after_deadline = "standby"

[domains.routing.pacing.accounts.cursor]
windows = ["monthly"]

[domains.routing.pacing.accounts.go]
windows = ["rolling", "seven_day", "monthly"]

[domains.routing.pacing.free.opencode]
policy = "max"
```

`provenant route pacing PROVIDER POLICY` sets a provider's default policy.
For a temporary or scheduled change, use for example
`provenant route override set --all --policy spend --until 1d`. Select a
single account with `--account` or a provider with `--provider` instead of
`--all`. Account selectors accept the same keys, entry IDs, labels and
`adapter/key` references as profiles; ambiguous references are rejected.
`--starts` and `--until` accept a duration in seconds, minutes, hours or days
from now, or an RFC 3339 timestamp; without `--until` the override stays until
cleared. `--window [DAYS@]START-END` (repeatable) limits it to hour windows,
such as `--window mon-fri@23-7`, in UTC or the IANA zone given by `--tz`. The
command echoes the rule it recorded and any rule it replaced, since each scope
holds one override. `provenant route override list` shows every override and
scheduled model policy rule, whether it is active now and the time remaining.
`provenant route override clear` removes every pacing override, or one scope's
with `--account` or `--provider`. Overrides are stored under `schedule` in the
same routing configuration using atomic writes, stop applying when they
expire, and expired rows are pruned on the next `set` or `clear`. Inspect the
effective profile and active overrides with `route list`;
its JSON `configured_accounts` keys and `reset_of` references use `adapter/key`.
`route resolve` names the governing source, window and deadline for each
pacing factor or exclusion. The console settings editor uses the same
validated configuration path.

Explicit fallbacks use `--fallback ADAPTER/ID,ADAPTER/ID`.

Dispatch uses the same route flags. Copy the resolved `adapter/model@effort`
from the attempt record; never infer a model from a tier or a provider name.
Quota observations use `provenant quota` and `provenant quota tick`.

Agy read-only lanes can run system commands, including Git, within the outer
read-only confinement profile. Headless tool permissions are auto-approved;
the profile still restricts writes to attempt scratch and private state. If
Agy reports a denied action, the terminal reason names it, for example
`provider denied: RunCommand`.

The routing, providers and quota crates define the records and eligibility
policy. Run `provenant help route` for flags.
