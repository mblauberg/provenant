# Model routing
Owner: provenant-routing
Anchor: tools/front-door/crates/routing/src

Run `provenant route list` to inspect the catalogue, configured pool weights
and pacing policy; pacing and readiness apply at dispatch. Use
`provenant --json route list` for records, `--adapter NAME` to narrow it, or
`--pool NAME` for one pool. Prefer `--route NAME` for a pool alias when dispatching work; use an exact
model when the task needs one. Resolve a pool with
`provenant route resolve --route NAME`, or select an exact model with
`--model ADAPTER/ID` and optional `--effort LEVEL`.
`provenant route resolve NAME` also accepts a route name or an unambiguous
model name. Unknown or ambiguous names report suggestions and examples.

An observed expired login projects as `login_expired` for routing; it does
not create a quota refusal. Existing scoped quota refusals retain their own
clearing rules. `provenant quota` shows the last reading and, when the account
registration is known, the login context for recovery.
Pre-launch fallback treats this reason as an authentication outage and uses the
same pin, budget, custody and launch-state guards as other authentication
failures. When there is no eligible fallback, the account is pinned or the
fallback budget is exhausted, the refusal says why. A launched or uncertain
effect is not repeated automatically.

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
members with uncalibrated partial windows are ineligible by default. Set
`routing.allow_unknown_partial_headroom` to `true` to accept unknown headroom;
observed exhaustion still blocks the route. Route diagnostics identify the
setting, its source and uncalibrated windows. Missing quota data uses
1.0. `spend` and `max` use 1.0; admission handles
provider quota refusals, and a paid route remains eligible while credits
remain. If no weighted member is ready, the first ready zero-weight member is
selected; otherwise a recoverable unavailable member is preferred, and
disabled members are excluded. If only disabled members remain, resolution
returns their exclusions. The receipt records candidate readiness and
exclusions. Operation ID and task index seed selection so retries repeat the
same choice. Only OpenCode `*-free` entries are marked free; Agy Gemini Flash
is a paid subscription model (see the
[catalogue](../../tools/front-door/crates/routing/assets/catalogue.toml)).

OpenCode quota refusals come from its structured quota and free-limit events,
and from stderr diagnostics naming `AI.Error.QuotaExceeded` or `usage limit
exceeded`. During a run, a line beginning with `AI.Error.QuotaExceeded:`
(after whitespace or ANSI control sequences) is treated as an authoritative
refusal and triggers cleanup even if the process might otherwise succeed. The
live rule also accepts an unterminated line; log-prefixed errors and generic
quota wording do not trigger it. At exit, stderr evidence is read from the
matching line and the one after it only if the adapter did not accept the run.
The refusal is scoped to `opencode-go`, or
`opencode-free` when that text names the free tier or free usage; a retry
interval with a unit (or a bare `retry-after` header value, in seconds) sets
the refusal expiry.

A graph node can move to its next configured route after the host verifies a
local preparation refusal and released custody. The earlier attempt remains in
the receipt history. Once a provider has launched, the graph does not switch
routes automatically for a writer; a confirmed read-only attempt may switch on
an authentication or usage-limit outcome within its retry budget. A quota-stopped
writer with an unfinished handoff may continue on another eligible account.

The settlement retry allowance is `domains.routing.settlement_retries`
(shipped default **2**). Set `retry.retries` on a preset or graph document for
its default, or on a node to override it: node → preset → host configuration →
shipped default. Zero disables settlement successors. For example, a preset
may declare `retry = { retries = 1 }`, and a leaf stage may declare
`retry = { retries = 0 }`. `fabric graph status ID --full` shows the effective
value and its origin. Admission seals the allowance; restart, same-key
observation, admission reprobes and verified pre-launch preparation fallbacks
do not renew or spend it.

`provenant quota` includes a recorded refusal's expiry in `resets_at` using
RFC 3339 and shows its time in the host's local zone. Human output adds the
local clock time (and weekday when the expiry is not today); a refusal without
a recorded reset stays exhausted without a displayed expiry.
Omit `--effort` on OpenCode free routes: their recorded model controls do not
support effort, so an explicit value is refused before launch.
`--confidential` excludes members and fallbacks that train on or retain prompts, or are unclassified.
Pool selection skips adapters that cannot support the requested write or network
mode; an unsupported explicit route is refused. Launch preparation checks the
selected adapter again. There is no classifier or route-synonym inference. Effort is set only when the
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

Writer cost advice uses exact model classes and ordered pool names from the
bundled `routing/assets/writer-advisory.toml` catalogue data. It does not infer
classes from model spelling. Replace those lists in the selected state's
configuration, for example:

```toml
[domains.routing.writer_advisory]
warn_models = ["claude/claude-opus-5-5"]
cheaper_models = ["claude/claude-sonnet-5-5", "codex/gpt-6.1-sol"]
pool_priority = ["design", "principal", "senior", "junior"]
```

The applied bundled pool takes precedence over `pool_priority`; exact models
and custom pools use the first matching bundled pool in that list. Advice
classes are independent of the provider evidence used for answering-family
assurance. Empty model lists disable the corresponding advice. Optional `label`
names the warning class; omitting it gives generic writer wording. The applied
model is always excluded from its own alternatives.

## Councils and retained evidence

`orchestrate` owns when to pair primaries or convene a council; HARNESS owns
which independent review legs count. The bundled pools include `utility`,
`junior`, `senior`, `principal`, `writing` and `design`. A named pool is a
weighted choice, not an ordered model list. Explicit route lists and
zero-weight fallbacks have their own ordering semantics.

Use `provenant run --preset review --prompt-file brief.md --explain`
to inspect roles and sources before dispatch. Inspect the recorded separation
and substitutions after dispatch. Different model names or requested families
alone do not establish independent answering families. Bind the resolved
`adapter/model@effort` from each attempt to the review evidence.

Attempts live under `<state>/runs/<run>/attempts/<attempt>/`. The default
state root is `~/.local/share/provenant/v1`; `.agent-run/` is a protected legacy
root. Keep full provider output in the attempt and use `provenant output TARGET`
for bounded reads. Prefer per-run controls to global configuration changes.

## Review shapes

Review shapes describe groups of read-only reviewers. Each group names a
routing pool and a count, and may assign one lens to each reviewer and request
distinct model families. Configure them in the selected state's `config.toml`:

```toml
[domains.routing.review.shapes.quick]
reviewers = [{ pool = "junior", count = 1 }]

[domains.routing.review.shapes.standard]
reviewers = [{ pool = "junior", count = 5, distinct_families = true }]

[domains.routing.review.shapes.deep]
reviewers = [
  { pool = "junior", count = 4, distinct_families = true },
  { pool = "senior", count = 4, distinct_families = true },
]

[domains.routing.review.select]
default = "standard"
by_risk = { routine = "quick", substantial = "standard", crucial = "deep", terminal = "deep" }
by_kind = { integration = "deep", docs = "quick", refactor = "standard" }
by_size = { small = "quick", medium = "standard", large = "deep" }
by_paths = { ".github/workflows/**" = "deep" }
```

The bundled shapes are `quick`, `standard` and `deep`. Tables layer over the
bundled configuration; a `reviewers` array replaces the earlier array. Each
count must be between 1 and 32, and a non-empty `lenses` array must have one
identifier per reviewer. Pools and selected shape names must exist. A shape may
dispatch at most 64 reviewers.

`distinct_families` is a preference. Reviewers in one group are labelled with a
shared `family_group`, and the scheduler picks routes outside the families their
siblings already use. When the pool has too few families it reuses one and
records the substitution; it never drops a lens or fails the review. Running
every selected lens is required, and model-family spread is best effort.

A lens tier is a pool name, an exact `ADAPTER/ID[@effort]` model or a route
table. Panel resolution keeps a tier as given and fills only an empty one from
the shape. A lens with its own tier takes no shape slot and joins no family
group.

The default size bands measure added plus removed lines, changed files and
crate roots in the diff from the merge base of the base ref to the subject.
Small is at most 150 lines, 3 files and 1 crate; medium is at most 1,000 lines,
15 files and 3 crates; larger or binary changes are large. Configure these limits
under `domains.routing.review.size`; each small limit must not exceed its medium
limit. The risk tier sets a floor. Integration heads select deep and are not
measured. An empty diff leaves the size unknown; a diff that cannot be measured
also leaves it unknown and reports git's error as a warning. Either way,
selection falls back to risk, kind and the default shape. A preset review of a
`head` or `node` subject, such as `deliver`'s, with no `size` or `review_shape`
is deferred: the graph carries one `review` node and the host measures the
pinned reviewed commit against `base` when that review starts, so the panel fits
the work actually written. The host journals the measured size, the shape and
the lenses it chose with the expansion, and a replay never measures again. Pass
`size` or `review_shape` to fix the panel when the graph expands. A graph has
one review base, so deferred reviews that name different bases are refused.

A read-only node on a pool name lists its alias twice. A typed usage refusal
that is confirmed to have stayed in local preparation can use the second entry,
excluding the model already tried. A launched attempt does not automatically
switch routes. An exact model, a route table with an adapter
and model, and any writer keep a single route. A route table without an alias,
or without an adapter and a model, is refused by name.

Lenses fill a shape's slots in the order given by `review.lenses.order`, without
repeats. The standard shape has five slots: `correctness-spec`, `tests`,
`design-fit-reuse`, `simplicity` and `reliability`. A kind can replace the order or add
lenses outside the slot cap, and risk tags add their conditional lenses:

```toml
[domains.routing.review.lenses]
order = ["correctness-spec", "tests", "design-fit-reuse", "simplicity",
  "reliability", "conventions-readability", "refactor-cleanup", "flexibility"]
[domains.routing.review.lenses.by_kind.docs]
order = ["conventions-readability"]    # one base lens, however many slots
[domains.routing.review.lenses.by_kind.refactor]
add = ["refactor-cleanup", "design-fit-reuse"]    # additive, deduplicated
```

Docs therefore run one readability reviewer at any depth, plus any risk-tag lenses.
A refactor runs its shape's base lenses plus `refactor-cleanup` and
`design-fit-reuse`. Risk tags (`security`, `concurrency`, `terminal`; a terminal
risk tier implies `terminal`) add `security-privacy`, `concurrency-data-errors`
and `adversarial`. A risk tier alone does not add `security-privacy` or
`concurrency-data-errors`; pass `--risk-tag` (or the `risk_tags` parameter) for
those. A caller-supplied `lenses` list in `review` keeps the caller's
order.

The bundled `review.lenses.by_paths` rules add `tests` outside the slot cap for
common test paths and runner config files. Changed diff hunks also add `tests`
when added or removed lines contain `#[test]`, `#[cfg(test)]`,
`#[tokio::test]`, `describe(` or `it(`. Repos replace the bundled table by
declaring their own patterns; an empty table disables the path rules:

```toml
[domains.routing.review.lenses.by_paths."**/specs/**"]
add = ["tests"]
```

The explanation and review report include the matched `by_paths:<pattern>`
reason. Marker-triggered additions report `test-marker`.

For crucial or terminal risk, the `tests` lens may attach optional mutation
evidence. From the PR head, write the workspace-relative diff to a temporary
patch and pass it to `cargo-mutants`:

```sh
base=origin/main
workspace=tools/front-door
git diff --relative="$workspace" "$base"...HEAD > "$TMPDIR/pr-head.diff"
(cd "$workspace" && cargo mutants --in-diff "$TMPDIR/pr-head.diff" --workspace)
```

Set the relative path to the Rust workspace under review, and keep the patch
relative to that workspace. Copy any required includes from outside the
workspace into it before the run. Skip tests that fail on the unmutated
baseline and list them as skipped. If `cargo-mutants` is unavailable or the
diff matches no mutants, report that and continue. Report mutant, caught,
missed and timed-out counts, wall time, and classify each survivor as a gap or
noise. Cap the run (`cargo mutants --timeout` per mutant and stop after about
15 minutes); report partial counts as timed-out or skipped, and still end with
the verdict. This evidence is optional and never gates delivery.

For example, review a commit with changed paths from a Git range:

```sh
provenant review HEAD --brief-file review-brief.md --risk crucial --risk-tag security --kind code --range origin/main...HEAD --explain
provenant --json review HEAD --brief-file review-brief.md --shape quick
```

The target must resolve to a commit. `--range` supplies changed paths and size for
selection; without it the range defaults to `<default branch>...TARGET` (the
branch `origin/HEAD` names, else `origin/main`, `main`, `origin/master` or
`master`), so size and path rules apply by default. Renames are treated as a deletion
and an addition, so rules can match either path. `--shape` overrides shape
selection and keeps size unknown; changed paths may still be read for lens
additions. The risk floor and integration override still apply. A changed path
that is not UTF-8 is rejected; use
an explicit shape when path selection cannot represent it.
`--wait` sets the overall wait limit, defaulting to one hour, and accepts
seconds or durations such as `30m`, from one second to four hours.

An explicit shape is applied first, then raised to the risk floor: the risk tier
sets a minimum that an explicit `--shape` or `review_shape` can raise but not
lower. Without an explicit shape, selection uses the most specific matching
`by_paths` pattern, then `by_risk`, `by_kind`, `by_size` and finally `default`.
Size and kind can raise the selected count above the risk floor, except where a
`by_paths` rule matched. Specificity is the
number of literal bytes, followed by fewer wildcards; lexical order breaks ties.
In path patterns, `*` and `?` do not cross directory separators; `**` does.
The selected shape and its source appear in the review summary. Family
distinctness is requested within each group; the summary records a substitution
when the pool cannot provide another family. Independent reviewers are submitted
without waiting for one another; a distinct-family group waits for each earlier
member's route before submitting its next member. Each reviewer goes through
normal dispatch, so pacing and quota apply per reviewer.

The summary collates each reviewer's run, route, state, verdict and full output
path without merging findings. Reviewers give findings followed by one final-line
declaration using the [review wire grammar](../specs/fabric.md#typed-node-outputs).
The prompt offers approval, change request or inconclusive; it does not offer
approval with nits. The collector uses the complete verified result stream;
duplicate declarations, an unrecognised token or an unreadable result is `unknown`.
JSON includes verdict counts, with planned
but `not_submitted` reviewers counted separately. The command
exits successfully only when every reviewer succeeds and approves; a failed,
inconclusive, change-requesting, approval-with-nits or unknown verdict is non-zero. This result
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

## Excluding members

An exclusion keeps a provider, an account or an exact model out of named-route
pools. It never widens a pool: if the exclusions leave a pool with no eligible
member, the dispatch is refused and the error names each exclusion and its
source (`flag`, `graph` or `config`).

A selector takes one of three forms:

| Selector | Example | Removes |
|---|---|---|
| Provider | `claude` | Every member served by that adapter. |
| Account | a label, an entry, or `claude/work` | That account's availability. A member of its adapter goes only when no other serving account remains. |
| Exact model | `codex/gpt-6-luna` | That model in every pool, including fallback members. |

An unknown selector is refused with the valid forms, before anything is
submitted.

For one dispatch, pass `--exclude` to `run`, `review` or `route resolve`. Repeat
the flag once per selector; it takes no comma-separated list:

```sh
provenant run --route junior --exclude claude --explain
provenant review --exclude codex/gpt-6-luna
provenant route resolve junior --exclude claude
```

`--explain` and `route resolve` list each exclusion that removed a candidate
and where it came from. `review` keeps its family separation: it substitutes
an unused family around the exclusions, and refuses once they exhaust the
families left, naming the exclusions in the refusal.

Three sources combine as a union, never as a replacement, and each rule keeps
its source in refusals and `--explain`:

| Source | Set by | Knob |
|---|---|---|
| `config` | The standing list in `[knobs]` | `routing.exclude` |
| `flag` | `--exclude` on this dispatch | `routing.flag_exclude` |
| `graph` | `--exclude` on a preset or automatically selected graph, inherited by every node | `routing.graph_exclude` |

A list that is not a list of strings, or holds an unknown selector, is
refused, the same as on the flag. `--explain` and `route resolve` read the
standing and flag lists the dispatch would use; a graph's list is applied only
when its nodes launch.

An exclusion applies to named-route pools, to the fallback models of a named
route, and to a named route that holds a single model. A pinned `--model`, and
the fallback listed alongside it, are never filtered.

To exclude an exact model from every pool until you remove it, use:

```sh
provenant route exclude add codex/gpt-6-luna --until 2h
provenant route exclude list
provenant route exclude remove codex/gpt-6-luna
```

`--until` accepts a future RFC 3339 timestamp or a duration; omit it to keep
the exclusion until it is removed. `route list` and `route override list`
also show these rules. Adding an exclusion also drops rows that have expired.

A stored row that cannot be read in full does not refuse the configuration.
If its model parses, the model stays excluded with no schedule; if not, the row
is ignored. Either way the repair shows as a `note:` on `route exclude list`,
`route list`, `route override list` and `--explain`, until you fix or remove
the row.

An explicit `--model` pin always selects the named model, whatever the
exclusions. Pacing overrides keep their precedence; an exclusion removes
members before pacing weighs the rest.

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
- `"2026-11-10T09:00"` for a one-off absolute time in the host's local time zone.

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

Current availability evidence is preferred. If only stale or unknown evidence
remains for an identified account, the lane warns and lets the provider check it.
An exhausted account remains excluded until its reset; an all-excluded route
reports the accounts and their remedies.

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
