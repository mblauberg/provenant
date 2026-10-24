# Telemetry and provider quota

Telemetry reduces supervised terminal attempt records into usage and
API-equivalent cost estimates. Its accepted contract is the
[Rust telemetry design](../specs/rust-port/design/telemetry.md).

## Rewrite status

Step 1 supplies the pure `provenant-telemetry` reducer and its typed
[TOML price asset](../../config/telemetry-prices.toml). It removes the Python
ledger, transcript and OTel ingestion, sources, delivery-cost joins, changes,
comparison and retro reports. The Rust product ships no collector or collector installer. Removing that
source does not alter an already installed LaunchAgent, collector binary,
harness setting or stored telemetry data. There is no history import or
transition adapter.

The command is pending CLI integration:

```text
provenant telemetry stats [--since DATE_OR_RFC3339] [--until DATE_OR_RFC3339] [--detail] [--json]
```

Until run integration lands, this command parses fully and returns
`OWNER_UNAVAILABLE` with exit 4. The old telemetry, stats and retro forwarding
routes are removed. Legacy quota, utility and Fabric peer paths still import
the removed telemetry owner; their owners remove those calls in later steps.

## Measurements and estimates

Run supplies validated `AttemptFact` values. The reducer selects `finished_at`
in `[since, until)`, with an optional project scope. CLI will parse dates as
midnight UTC and require an offset on timestamps. Every selected retry
contributes usage once; task outcomes use the highest selected attempt number.
Active attempts come separately from run's `snapshot`.

Usage has five disjoint categories: uncached input, cache read, cache write,
output excluding reasoning, and reasoning. Missing categories remain unknown.
Each sum includes its missing count; a measured zero needs no price. Groups use
exact resolved provider/model/effort/account labels, with missing keys last.
Detail adds UTC daily account groups. Requested routes and parent references do
not add usage or supply missing identity.

The price asset contains exact provider/model entries with recorded source
references and a review date. Rates are integer USD microdollars per million
tokens. Omitted rates are unknown; zero rates require explicit verification.
Reasoning uses the reviewed output rate once. There are no model regexes,
account inference, runtime price fetches or per-user price overlays. State owns
bundled asset loading; telemetry validates the schema.

Costs use checked integer arithmetic and round half-up once at output. Reports
include a nullable complete estimate, the known-cost lower bound and the number
of attempts with missing cost. Subscription routes without prices are unpriced.
These are API-equivalent standard-rate estimates, not bills; context, peak-hour
and cache-TTL adjustments are outside the recorded price table.

Coverage is `supervised_terminal_attempts` on the selected host/project/window.
Unsupervised activity, old-ledger history and other hosts are outside it.
Missing data never implies zero use. A failed read or reduction produces no
partial totals. Reduction refuses more than 100,000 selected attempts, 10,000
combined groups or 1 MiB of retained group keys. CLI will enforce its separate
1 MiB envelope limit and bounded human output.

## Collector retirement

Source deletion does not uninstall an existing collector or edit harness
configuration. Use the stable Rust binary and the explicit `provenant install`
procedure in the [front-door runbook](front-door.md) to preview and apply
selected harness changes. Stop selected clients and config editors through
`check install`; pass `--legacy-checkout` only for exact entries from that
checkout. The installer does not discover launchd services or delete paths that
are absent from its printed plan. Retiring an already installed collector needs
separate host-specific authorization and verification. Preserve old ledger,
transcript and collector data.

## Provider quota

The following is the legacy quota owner's reference during the rewrite. Its
telemetry imports, state paths and renderer are unavailable after step 1 removes
the ledger. Quota and execution-route own their replacements; this telemetry
step does not change account eligibility, probes or cooldown policy. Ledger
window history and plan-change events are retired.

```sh
scripts/provenant-quota                 # fetches only the accounts that are due
scripts/provenant-quota --json
scripts/provenant-quota --refresh       # explicitly bypass the schedule
scripts/provenant-quota tick --json     # fetch what is due, write report.json, print it
scripts/provenant-quota schedule        # each account's next due time and reason; never fetches
scripts/provenant-quota trace --since 24h
scripts/provenant-quota path [--json]   # the quota state directory
scripts/provenant-quota forget agy/google-<hash>   # drop a signed-out account's remembered reading
scripts/model-route routes --json         # cached quota only; never fetches
```

Codex prefers the credential-free `codex app-server` JSON-RPC rate-limit read,
then falls back to the latest local rollout observations already ingested for
the same account. Claude reads its own OAuth usage endpoint using the
identity's credentials in process. On macOS it falls back to the identity's
Keychain item, which Claude Code names `Claude Code-credentials` for `~/.claude`
and adds an 8-character path hash for any other config directory. The first
read of each item may ask you to allow access. Cursor reads its usage-summary
endpoint using its local session.
Authenticated HTTP requests refuse redirects, so credentials stay at the
provider endpoint. Tokens never enter the ledger, cache, logs, receipts or
stdout and are never shown to agents. The owner authorised this credential use
for this feature on 2026-10-21. Development and tests use fixtures only.

agy answers `agy -p /usage --output-format json`, which reads its quota summary
without an agent turn or quota spend; the CLI keeps its credential. Each model
pool reports a weekly and a five-hour bucket, shown as `gemini.weekly`,
`claude.5h` and so on; a bucket agy marks disabled is unreported. One Keychain
item serves every Google account agy signs in to, so a discovered agy account
is labelled `google-<hash>`: a hash of the signed-in address from agy's newest
log under `~/.gemini/antigravity-cli/log`. A reading whose own run reports
another address is refused rather than filed under the wrong account. agy quota
is report-only: its pools differ per model family, so it sets no provider-wide
cooldown (#1062). Each window carries its `pool`, such as `gemini` or `claude`;
see Display below.

Switching agy to another Google account keeps the previous one as
`status: signed_out`, with its last reading, observed time and resets. A
signed-out account is never fetched, has no local activity and never enters
routing placement. Signing in again resumes the same label and history. The
label is a hash of the address, so it is the same on every host. `provenant
quota forget <provider>/<label>` deletes a signed-out account's remembered
snapshot; it refuses an account that is signed in. `quota.remembered.forget_after_days`
(default unset) stops listing a remembered account unseen that long, without
deleting it. Listing configured agy fingerprints in `accounts.agy` names them
and still shows the signed-in and remembered ones not listed. An alias such
as `accounts.agy.work` binds to the account it first reads, decided under the
refresh lock, and keeps that account's history when another signs in: a
reading for another account, or one that does not say whose it is, fails as
`auth` and leaves the last reading `stale`. To rebind an alias deliberately,
set its `fingerprint` to the new account's label, such as
`"work": {"fingerprint": "google-4e7d2a91c05b"}`. The mechanism is
generic: a provider module with `fingerprinted(label)` gets it. Codex and
Cursor are labelled `default` and do not use it yet: Codex's rate-limit read
carries no account identity, so it would need a further `account/read` call,
and Cursor's user id is only in the session token it reads at fetch time.

`PROVENANT_QUOTA=0` or `quota.enabled=false` in the telemetry config disables
fetching and quota routing input. Cached quota can still be displayed. Errors
retain the last successful observation with `status: stale`; failed attempts
back off by error kind, as below. The cache lives beside the ledger under
`quota/<provider>--<account>--<host>.json`.

### Refresh schedule

Each account is due on its own schedule. It is fetched sooner while its identity
directory shows recent use (`active`), less often when idle, sooner still when a
window nears exhaustion or will run out before its reset, and not at all while
a window is exhausted until just after its reset. Every report row carries a
`schedule` block with `next_due_at`, `reason`, `ttl_seconds`, `trigger`,
`failures`, `backoff_until`, `error_kind`, `last_duration_ms` and
`source_kind`; the report adds the earliest `next_due_at` and `refreshing`, the
`provider/account` ids being fetched now. Activity is read from file times only.

`scripts/provenant-quota tick` takes `--trigger timer|panel|manual|event|wake|power` and
`--power ac|battery|low`, supplied by the caller. It holds
`quota/refresh.lock` without waiting: a second tick prints the current report
and fetches nothing. Up to `max_parallel` accounts fetch at once; agy runs alone.
`quota/report.json` is rewritten atomically after each fetch. When nothing is
due, tick fetches nothing and rewrites the report only if `next_due_at` moved.
A `manual` trigger fetches an account at least 30 seconds old and overrides a
backoff other than `rate_limited`; `--ignore-backoff` overrides any backoff.
A `power` trigger never fetches.

`quota.schedule.enabled=false` restores the fixed period: an account is fetched
when its last attempt is older than `quota.cache_seconds` (at least 300). With
the schedule on, `cache_seconds` is unused.

| `quota.schedule` key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Use the adaptive schedule |
| `min_interval_seconds` | 60 | Shortest wait between fetches of one account |
| `max_interval_seconds` | 21600 | Longest wait |
| `active_window_seconds` | 900 | Identity use this recent makes an account active |
| `jitter_fraction` | 0.1 | Spread of each wait, stable per account |
| `coalesce_seconds` | 120 | Fetch an account due this soon with the others (at most a quarter of its TTL) |
| `near_exhaustion_percent` | 85 | Use at which the wait shortens |
| `near_exhaustion_factor` | 0.5 | Multiplier then, and when run-out falls within the horizon |
| `runs_out_horizon_factor` | 2 | Horizon as a multiple of the TTL |
| `reset_grace_seconds` | 60 | Wait after a reset before an exhausted account is checked |
| `exhausted_recheck_seconds` | 21600 | Longest wait while exhausted |
| `battery_factor` | 2 | Wait multiplier on battery |
| `low_power_factor` | 4 | Wait multiplier in low power |
| `low_power_skip_costs` | `["expensive"]` | Costs that wait the maximum in low power |
| `manual_min_age_seconds` | 30 | Youngest reading a manual trigger refetches |
| `event_debounce_seconds` | 30 | Youngest reading an event trigger refetches |
| `max_parallel` | 3 | Concurrent fetches |

Per provider, `quota.providers.<name>` or an account's own `quota` object sets
`ttl_active_seconds`, `ttl_idle_seconds`, `ttl_panel_seconds` (the youngest
reading a panel open refetches), `cost` (`cheap`, `moderate` or `expensive`)
and `solo`, and may override any key above. The defaults:

| Provider | Cost | Active | Idle | Panel |
|---|---|---|---|---|
| claude | moderate | 300 | 1800 | 120 |
| codex | moderate | 900 | 3600 | 300 |
| cursor | cheap | 1800 | 3600 | 600 |
| opencode | cheap | 300 | 3600 | 60 |
| agy | expensive, solo | 1200 | 7200 | 900 |

An invalid value falls back to the next layer, and the account's `schedule`
block lists it under `notes`.

A failed fetch sets `error_kind` and waits before the next attempt:

| `quota.backoff` key | Default | Applies to |
|---|---|---|
| `base_seconds` | 120 | `rate_limited`, `network` and `other`: doubles per consecutive failure, with jitter |
| `max_seconds` | 3600 | Cap on that doubling |
| `auth_seconds` | 3600 | `auth`; cleared early by identity use, such as signing in again |
| `missing_seconds` | 86400 | `missing`, such as a CLI that is not installed |

Each wait runs from when the attempt finished. A `Retry-After` header lengthens
a `rate_limited` wait, up to a day; a date in it is kept as that deadline. Trace
and state files are created 0600 and never written through a symlink. If every
fetch in a tick fails to connect, the report sets `offline: true`.

Each attempt appends one line to `quota/fetches.jsonl`: `at`, `provider`,
`account`, `trigger`, `outcome`, `error_kind`, `duration_ms`, `retry_after_s`
and a scrubbed `error`. `scripts/provenant-quota trace --since 24h [--json]` totals
fetches, failures by kind and duration percentiles per account. Lines older than
`quota.trace.retention_days` (default 30) are pruned at most once a day.

Claude's `claude auth status` result is cached per identity under
`quota/memo/` for a day, or until the identity is used. OpenCode reads its v1
message table through its session and time index, one time range per session,
filtered by message time.

Configure accounts in `<instance-root>/config/telemetry.json`. These are the
`telemetry.accounts.<provider>.<label>` settings; omit the outer `telemetry`
object in this dedicated file. Labels are short non-secret names, never email
addresses or tokens. Plan labels are optional display text; priority is an
optional integer, with smaller numbers preferred.
An entry without `identity_dir` means the provider's default directory.
`aliases` lists other directories signed in to the same account, such as the
CLI default when it holds the same login as a named directory. They affect
only ledger attribution, never quota fetching or routing.

```json
{
  "quota": {"enabled": true, "schedule": {"max_parallel": 3},
            "providers": {"agy": {"ttl_idle_seconds": 14400}},
            "window_labels": {"iguana_necktie": "Cloud session credits"},
            "credits_policy": "last_resort", "credit_floor": 0},
  "accounts": {
    "claude": {
      "max-a": {"identity_dir": "~/.claude-a", "aliases": ["~/.claude"], "plan": "Max", "priority": 1},
      "max-b": {"identity_dir": "~/.claude-b", "plan": "Max", "priority": 2}
    },
    "agy": {
      "work": {"plan": "work"}
    },
    "codex": {"work": {"identity_dir": "~/.codex"}}
  }
}
```

The agy example is this Mac's account. Its sign-in lives in the shared macOS
login Keychain: another `identity_dir`, `AGY_CONFIG_DIR` or HOME on this Mac does
not isolate a second account. Configure the personal agy account on the workshop
Mac, for example `"agy": {"personal": {}}` in that host's telemetry config.
Give the Macs distinct `local_host` ids in `.agent-fabric/hosts.json`; host scope
keeps their cooldowns apart. No agy HOME switching is performed.

A provider absent from config gets one discovered default account when its
identity directory exists. An explicitly empty provider object disables that
auto-discovery. Disabled or removed accounts disappear from the report and
routing input; their history remains. There are no built-in plan quotas or
window schedules. A missing window is unreported, never 0%.

Each window shows provider-reported use, length and reset time. Pace compares
actual use with even use since the reported window start, projects run-out and
states whether use lasts until reset. Pace is recomputed at report time from
the latest observation. `use_it_or_lose_it` reports the current unused fraction and projected headroom
expiring at the longest reported reset. Each window also reports headroom.
Usage of 100% or more is exhausted even when timing is missing or inconsistent.
Only zero use can mean a window has not started. Positive use with a missing or
passed reset has unknown pace pending a fresh reading; it retains its reported
headroom. A reset up to five minutes beyond the window length is clamped for
pace calculations; larger discrepancies have unknown pace.

Some providers report budget windows in money rather than a percentage. Claude
names them by codename, such as `iguana_necktie` with `limit_dollars` and
`used_dollars`. Such a window keeps its percentage for pace and adds
`used_amount`, `limit_amount` and `currency`; the table shows `$136 of $250`.
`quota.window_labels` maps a window name to a display label and adds to or
overrides the built-in map, which labels `iguana_necktie` as
`Cloud session credits`. Every report window carries `label`; see Display.

An account can also report `usage_credits`, a list of credit balances drawn on
once the plan allowance runs out, and `resets`, the banked resets available with
their known expiries. Each credit has `kind`, `balance` (null when `unlimited`),
`currency` or `unit`, `expires_at` and `source`. `resets` has `available`, a
sorted `expires_at` list that may be shorter than `available`, and `source`.
Both are omitted when the provider reports none, zero or nothing; a missing
balance is unknown, not zero. The table adds one line per account that has
either, for example `codex/default: 27,406 credits available; 1 banked reset
(expires 7 Nov)`. Codex reports both through `account/rateLimits/read`. Claude
reports a `spend.balance`, shown when positive, and no banked resets. A reset
recorded with `scripts/provenant-quota credit` appears with `source: manual` when the
provider reports no count.

The legacy quota classifier distinguishes `natural`, `granted_date_kept`,
`granted_window_restarted`, `window_moved`, `not_started` and `window_started`
changes. Natural rollover compares the new provider window start against the
previous provider reset. Restarted windows must start within five minutes of
the observation. These classification rules remain with quota; their retired
ledger events provide no history in the Rust rewrite.

### Display

The report owns the display vocabulary, so the table and the menu bar agree.
All of it is additive to `provenant.quota.report.v1`.

- Each account has `provider_name` ("Claude", "Codex", "OpenCode",
  "Antigravity"; an unknown provider is title-cased), `display_name` and
  `plan_label` ("Pro Lite", "Pro (Student)").
- `display_name` comes from an optional `display_name` key on
  `accounts.<provider>.<label>` in `telemetry.json`. Without one, a label is
  title-cased (`anese` reads "Anese"), `default` has none, and a fingerprint
  label such as `google-<hash>` has none, so the provider name stands alone.
  Labels themselves never change.
- Each window's `label` comes from `quota.window_labels`, else from its length:
  `5-hour`, `Daily`, `Weekly`, `Monthly` (28 to 31 days, so a billing cycle
  reads Monthly), or `3-day` for another length, with a qualifier when the
  name adds one, such as `Weekly (Opus)`. An untimed window is its name in
  sentence case.
- A `limits` entry that restates a named window, with the same pool, length,
  use and reset to the minute, such as Claude's `weekly_all:2` beside
  `seven_day`, is dropped; the named one stays. An entry that differs in any
  of those stays, and two named windows are never merged.
- A pooled window has `pool` and `pool_label` ("Claude models"). Pools are
  independent: the account is usable while any pool is, the most used usable
  pool binds, and when every pool is exhausted the account is `back_at` the
  first pool's return.
- `display_window` names the window to show: an exhausted one; else one at or
  above `quota.display.short_window_alert_percent` (default 90, echoed in the
  report's `display`); else the longest. A window carries `reset_passed: true`
  and never shows as current use when its reset passed after `observed_at`, or
  when the reading is fresh but the window has reset below 95% and is not
  refused (by name, or by an unnamed refusal). Resets allow five minutes for
  clock skew before counting as passed; the table shows `-` and "reset since".
- `headline` names the account the menu bar shows, set by
  `quota.menubar.headline`: `recent` (default), the usable account with the
  latest local activity, or the highest priority without any; `worst`, the
  highest binding use; `<provider>/<account>` to pin one; or `none`. Its
  `account` is null when nothing usable remains.

```json
{"quota": {"display": {"short_window_alert_percent": 90},
           "menubar": {"headline": "recent"},
           "remembered": {"forget_after_days": 30}},
 "accounts": {"claude": {"anese": {"identity_dir": "~/.claude"}},
              "agy": {"google-<hash>": {"display_name": "Michael"}}}}
```

### Plan prices and routing advice

Plan prices are user configuration, read at report time and never fetched.
List them under `plans.<provider>.<plan>` in the same file. `<plan>` matches a
reading's `plan_tier`, then `plan_detected`, then `plan`, ignoring case and
punctuation; `*` covers any reading of that provider with no other match. Each
entry is a list of monthly prices with the date each took effect; the latest
`effective` date at or before a reading applies. `list_usd` is the advertised
price and `paid_usd` what you pay, so a free student plan has a list price and
`paid_usd: 0`. Value for money divides by `paid_usd`; upgrade comparisons can
use `list_usd`. A price applies per account, so two accounts on one plan count
twice:

```json
{
  "plans": {
    "claude": {
      "default_claude_max_20x": [{"list_usd": 200, "paid_usd": 200, "effective": "2026-01-01"}],
      "pro": [{"list_usd": 20, "paid_usd": 20, "effective": "2026-01-01"}]
    },
    "cursor": {"pro_student": [{"list_usd": 20, "paid_usd": 0, "effective": "2026-01-01"}]},
    "opencode": {"*": [{"list_usd": 10, "paid_usd": 10, "effective": "2026-09-01"}]}
  }
}
```

The values above illustrate the shape only. Enter the amounts you actually pay.
Mileage (API-equivalent USD per 1% of a window), upgrade gain and value per plan
read these tables; see #1071.

```sh
scripts/provenant-quota credit codex --account work add
scripts/provenant-quota credit list --json
scripts/provenant-quota credit codex --account work use
```

This example banks one Codex reset credit. `use` records a credit the user has
already consumed; it never redeems anything with a provider. Credit records are
local advice, keyed by provider, account and host. Existing two-part
provider/account keys remain host-agnostic when updated; an exact host key takes
precedence when present. Agents never redeem credits.
Advice is to hold until near exhaustion, wait if the natural reset is within
six hours, and prefer a reset that keeps the date when that date is days away.

Only the currently selected CLI identity affects routing (`CLAUDE_CONFIG_DIR`,
`CODEX_HOME`, or the provider's default directory). Changing that identity is
reflected on the next routing read, including when snapshots are cached. Cooldowns use the same account key and host as quota. See
[model routing](model-routing.md) for identity sources and scoped reset. Cache
and lock paths include the resolved identity directory; older unbound caches
require a fresh reading before they can affect routing. The report orders other
configured accounts by priority and reset proximity as
placement advice; it never switches credentials. An exhausted selected account
blocks that adapter until the latest exhausted window resets. A newer provider
reading with a reset date updates that account and host's adapter cooldown. Exhaustion without a
reset date preserves existing usage cooldowns. Ahead-of-pace, reserve, on-pace,
early and not-started readings clear older contradictory usage cooldowns in the
same account and host only when allowance lasts until reset or use is below 95%.
Unknown readings preserve them. Short-term `rate_limited` cooldowns remain.
Usage above even pace with allowance remaining is `ahead_of_pace`, not a shortage.
The summary estimates how long the allowance lasts at the observed rate, then
includes usable credits and banked resets when allowance would run out first.
Credit hours require a measured burn rate while credits are being drawn;
otherwise, the balance is shown with `credit runway unknown`. Banked resets are
advice only; agents never redeem them. Pace does not lower routing weights.
Account selection ranks a known weekly shortage below an unknown cache unless
usable reserves cover the estimated gap to reset. Exhaustion without usable
credits still blocks routing, and `credit_only` keeps its policy below.
Readers retain `deficit` for old receipts; new readings do not emit it.

A window is stale only when `observed_at < reset_at <= now - 300 seconds`: its
reset passed after the reading, allowing five minutes for clock skew. In a fresh
reading, a window with a past reset binds while use is at or above 95% or it is
refused, by name or by an unnamed refusal. A refusal code that names no reported
window binds like an unnamed refusal in a fresh reading. Otherwise the remaining
windows decide, and an account with no current window left reads
`unknown` until a fresh reading arrives. A cached refusal expires with its named
window, or, when no window is named, after every reported window has reset and
none is unreported. Spend caps and refusals for unreported windows remain.
Other current windows still bind.

An account whose allowance is spent can stay usable on credits. The allowance is
spent when a window is exhausted or the provider reports `ordinary_usage_allowed:
false`. Credits keep the account usable when the latest reading has a balance
above `quota.credit_floor` (default 0), or unlimited credits, and the provider
reports no spend control reached and no depleted credits. Such an account has
state `credit_only`, `drawing_credits: true`, a summary with its balance, and
`credit_burn_per_hour` once successive readings show the balance falling. It adds
no quota cooldown and clears older cooldowns that quota set. A cooldown from a
real refusal stays until it expires or a reading shows allowance again, since
credits may be switched off. A spent allowance without usable credits is
`exhausted`, even below 100%, and cools until the governing window resets. With
no future reset time known, it cools for `quota.unknown_reset_cooldown_seconds`
(default 3600) from the reading, and the next reading decides.
`quota.credits_policy` sets how routing ranks it:

- `last_resort` (default): placement ranks it after every account with
  allowance, and pools skip its adapter while another entry with allowance
  qualifies. A request that names the adapter or the model still uses it.
- `normal`: it ranks like an account with allowance.
- `never`: it stays exhausted and cools until reset.

A balance that the latest reading does not report is unknown, never usable.

### Adding a provider

1. Add one small module under `scripts/quota_providers/`, then one entry in
   `provider_quota.REGISTRY`. No provider-name branches belong in the core.
2. Implement `ADAPTER`, `default_identity()` and `fetch(account, io, now)`.
   Fetch returns `windows`, optionally `plan`, `source`, `unreported`, `credits`,
   `usage_credits`, `resets` and `observed_at`. Provider-specific account config keys are passed through
   in `account.options`; `identity_dir`, `enabled`, `plan` and `priority` are
   handled by the core. Options stay out of snapshots and reports. Each window
   has `name`, `used_percent`, `window_minutes`, `reset_at` and `raw_kind`; names
   are free strings. Report unknown lengths or
   resets as null. Core code normalises, caches and computes pace. When one
   credential store serves several accounts, `account_label()` may name the
   signed-in one from local, non-secret state; it must never fetch.
3. Use the bounded `io` HTTP/process functions and fixed, sanitised failures.
   Return only quota metadata. Keep tokens inside the provider call chain.
4. Register a fake module and mock all process/network boundaries in tests.
   Add the account to config and verify it appears in `scripts/provenant-quota`.

OpenCode has no usage or limits endpoint and documents no reset semantics.
Go limits are plan-wide dollar amounts (5 hour 20%, weekly 50%, monthly 100% of
the monthly amount). The opencode provider is a local estimate: it sums paid
`opencode-go` cost from both `opencode.db` message stores (v1 `message`, v2
`session_message`; read-only, rolling windows by message timestamp) against a
monthly cap calibrated from the Go dashboard, not documented: $32 for Go, which is
assumed when no `plan` is set (the source says so). Set `limits.monthly_usd` to
override it or to cover another plan, which is otherwise unreported. The estimate
is partial: other hosts using the same account are not counted, so it
undercounts. A recent `GoUsageLimitError` in the store is authoritative: that
window reads 100% until its estimated reset (the error time plus the window).
Set `limits.cycle_anchor` (ISO datetime of the subscription cycle start) to use fixed
weekly (every 7 days) and monthly (same day each month, clamped to short months)
cycles instead of rolling ones: spend is summed within the current cycle, resets
are the next boundary, and an error expires at it. Set `limits.weekly_anchor` when
the weekly cycle starts at a different time from the monthly one; it defaults to
`cycle_anchor`. The 5 hour window always rolls.
Free zen models are ignored. Only message tables are read, never credentials.

```json
"opencode": {"go": {"plan": "Go", "limits": {"monthly_usd": 32}}}
```

agy usage remains receipt-only; its quota comes from `/usage` above. A
configured agy label names whichever account is signed in, so leave agy
unconfigured to keep its accounts apart. The inspected Cursor reference uses
its SQLite store for authentication, then obtains allowance from the endpoint;
it does not supply a local token-usage ledger. Cursor usage tokens therefore
remain receipt-only. Neither missing coverage is represented as zero use.
