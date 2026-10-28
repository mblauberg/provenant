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
affect pacing.
Missing quota data uses 1.0. `spend` and `max` use 1.0; admission handles
short rate-limit cooldowns, and a paid route remains eligible while credits
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

The bundled Rust catalogue (`tools/front-door/crates/routing/assets/catalogue.toml`)
owns default pool membership, weights, pacing and effort controls. Override a
member with `provenant route set POOL MODEL [--weight N] [--effort E]
[--efforts a,b,c]`, remove one with `provenant route remove POOL MODEL`, or
restore a bundled pool with `provenant route reset POOL`. Set provider pacing
with `provenant route pacing PROVIDER pace|spend|max|off`. These edits are stored
under `[domains.routing]` in the selected state's `config.toml`. Catalogue
presence does not establish adapter availability; readiness is decided at
dispatch, so inspect the resolved route recorded for the attempt.

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
window selectors and deadline forms are validated together; unknown keys are
rejected with the valid options.

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

For a temporary change, use for example
`provenant route pacing override --all --policy spend --until 1d`. Select a
single account with `--account` or a provider with `--provider` instead of
`--all`. The expiry accepts a duration in seconds, minutes, hours or days, or
an RFC 3339 timestamp. `provenant route pacing override clear` removes the
overrides early. Overrides are stored with their expiry in the same routing
configuration using atomic writes, and stop applying automatically when they
expire. Inspect the effective profile and active overrides with `route list`;
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
