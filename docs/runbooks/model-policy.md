# Model policy
Owner: provenant-routing
Anchor: tools/front-door/crates/routing/src/policy.rs

Bundled model policy is embedded from
`tools/front-door/crates/routing/assets/model-policy.toml`. It uses the same
`ModelPolicyRule` selectors, schedules and overlays as configuration, and is the
single source for DeepSeek's peak schedule and multiplier. Telemetry's price
table records base rates only.

Model policy overrides live under `[domains.routing.model_policy]` in the selected state root's
`config.toml`. It supplies compaction and cache planning advice, pool preferences,
lane guidance and enforced result checks. Inspect the selected policy and its
sources with `provenant route resolve MODEL`; `--account KEY` previews an account's
policy. Account selectors in rules use a registered key, preferably `adapter/key`.

Layers are bundled defaults, user defaults, then matching rules ordered by
scope: global, adapter, model, account, and model plus account. An adapter makes
a rule more specific within its scope. Later rules win at the same scope.
Only supplied fields replace earlier values; an empty `sections` array clears
section requirements. Set `bundled = false` to start without bundled policy.
Rules support inclusive starts, exclusive expiries and hour windows in the
schedule's optional IANA `tz` zone (UTC by default). An overnight window wraps
midnight, including across weekday boundaries. Inactive rules have no effect. Put temporary
overrides after enduring rules of the same scope.

`model_suffix` matches the end of the provider-native model ID. Combine it with
`adapter` to scope open-ended classes, such as OpenCode's `-free` routes.
It has model scope for precedence. Supplied selectors are combined; malformed
suffixes drop the rule, or refuse configuration when it enforces a restriction.
For project-specific policy, use the project's selected state configuration;
add `account` to limit any override to one account.

```toml
[domains.routing.model_policy]
bundled = true

[domains.routing.model_policy.defaults.cache]
read_share_target = 0.95

[[domains.routing.model_policy.rules]]
model = "claude/claude-sonnet-5-5"
account = "claude/work"
[domains.routing.model_policy.rules.policy.compaction]
window = 300000
mode = "task_boundary"
min_turns = 14
[domains.routing.model_policy.rules.policy.lane]
max_context = 800000
max_turns = 100

[[domains.routing.model_policy.rules]]
model = "opencode/opencode-go/deepseek-v4.1-flash"
[domains.routing.model_policy.rules.schedule]
expires = "2026-11-01T00:00:00Z"
windows = [{ weekdays = [0, 1, 2, 3, 4], start_hour = 1, end_hour = 4 }, { weekdays = [0, 1, 2, 3, 4], start_hour = 6, end_hour = 10 }]
[domains.routing.model_policy.rules.policy.cache]
price_multiplier = 2.0
read_heavy_weight = 2

[[domains.routing.model_policy.rules]]
model = "agy/gemini-3.8-flash"
account = "agy/work"
[domains.routing.model_policy.rules.schedule]
expires = "2026-11-01T00:00:00Z"
[domains.routing.model_policy.rules.policy.result]
sections = ["## Findings", "## Evidence"]
fallback = "codex/gpt-6-luna"
fallback_effort = "high"
```

`starts` and `expires` are quoted RFC 3339 timestamps in any offset; an
unquoted number is the wrong type and drops the rule. Schedules may set `tz`
to an IANA time zone; omitted `tz` uses UTC. Weekdays use Monday-based indexes, Monday `0` through Sunday `6`;
an empty `weekdays` list matches every day. Window end hours are exclusive and
overnight windows wrap midnight. Schedules are validated at load: `starts`
must precede `expires`, `tz` must name an IANA zone, and each window needs
distinct start and end hours and distinct weekdays, with at most 16 windows. A
rule with a malformed schedule or selector, including a value of the wrong
type, is dropped with a warning, or refuses the configuration when it carries
an enforced lane restriction.
`provenant route override list` shows policy and pacing schedules, whether
each is active and the time remaining to expiry.

The timing keys that predate `schedule` are not converted on load. A pacing
override or policy rule that still carries one is dropped with a located
warning that names the replacement, such as
`domains.routing.pacing_overrides.0: ignored invalid configuration (domain: moved_to_schedule.expires)`;
other records still load. A rule that enforces a lane restriction refuses the
configuration instead, at the retired key. While any retired key remains,
every configuration write, including `provenant accounts register`, is refused
and names the same repair.

Run `provenant config migrate` to rewrite them in place. `--dry-run` reports
what would change without writing, and `--json` returns the report.
`starts_unix_seconds` becomes `schedule.starts`, `expires_unix_seconds` becomes
`schedule.expires` (each as an RFC 3339 timestamp) and a top-level `windows`
becomes `schedule.windows`. A pacing override whose expiry has passed is
dropped, since it can no longer apply; a policy rule is converted and kept. The
command saves the previous file as `config.toml.bak-<UTC timestamp>` in the
state directory before it writes, keeps comments and layout elsewhere in the
file, and refuses, changing nothing, when a record already sets the
`schedule` key it would write. A comment attached to a dropped override goes
with it.

The bundled policy follows issues #2177 and #2185 and the repository's price
table. These are adjustable operating defaults:

| Route | Policy |
|---|---|
| Claude | 275k task-boundary window after at least 14 turns; Opus/Sonnet 5.5 cache read 0.05x, write 1.25x and output 5x input |
| Codex Sol/Terra | Cache read 0.05x, write 1.25x and output 5x input |
| Luna, Haiku 5.5 and Sonnet 5 | Cache read 0.10x |
| DeepSeek v4.1 Flash on Go | 1M context, cache read 0.02x, no cache-write fee, output 4x input, read-heavy multiplier 3; 800k task-boundary window after at least 20 turns |
| DeepSeek weekday peak | 2x price multiplier at 01–04 and 06–10 UTC, reducing its read-heavy preference to 1.5x |
| Terra | Flagged as prone to stopping early; up to two continuations when it explicitly reports unfinished work |
| Gemini Flash and OpenCode free routes | Read-only; non-empty result with exact `## Findings` and `## Evidence` section lines; one fallback to Luna at high effort |
| All models | Advisory cache-read share target 90% |

Free routes are absent from bundled pools. Exact IDs and user-configured pools
can still use them. Change result checks or lane restrictions for one account,
including with an expiring rule.

Use `provenant run --route junior --read-heavy ...` for surveys and triage.
The equivalent knob is `routing.workload`, with values `general`, `read_heavy`,
`edit_heavy` and `review`. Review dispatches identify themselves automatically.
`route resolve --read-heavy` shows the preference in the candidate receipt;
`--writes` previews write eligibility. The preference applies only to models
with a read ratio at most 0.03 and at least 1M context. It multiplies existing
weights and pacing while retaining readiness, quota, disclosure, effort and
confinement checks. Edit-heavy work gets no cache preference.

Compaction modes are `auto`, `task_boundary` and `never`. Claude launches receive
`CLAUDE_CODE_AUTO_COMPACT_WINDOW` for a configured window unless mode is `never`.
Task-boundary mode asks the model to compact deliberately while the cache is warm
and prefer a fresh lane with a handoff over repeated compaction. For models
without an explicit per-model window, chair guidance is about 250–300k; do not
lower it below about 150k without evidence. Use the per-model windows in the
table above where specified. `never`
suppresses deliberate compaction guidance and the window override; native
provider safety compaction may still occur.

`route resolve` reports compaction break-even for a 15k summary and zero rework:
`N* = (rC + (o+w)S + wR) / (r(C-S))`, in units of uncached input price. Missing
prices and zero read prices produce an unknown estimate. At the bundled Claude
window this is about 8.27 future turns; rework raises it. This is advice for
model judgement, not an automatic compaction trigger.

Fallback model IDs and efforts are checked against the bundled catalogue when
the host configuration loads, so an invalid fallback is reported before a run
reaches the result-check retry.

Cache `ttl_seconds` and `min_prefix`, and lane `max_context` and `max_turns`, are
planning guidance supplied to the model. They do not change a provider's cache
TTL or impose a process kill. Providers without an evidenced launch control
receive guidance only. `stop_early_ok = false` asks for the shared schema v2 `handoff` fence. A
`stopped_early` declaration can continue the retained session, or start fresh
on the same worktree when resume is unavailable, with `max_continuations`
bounded to eight. `done` and `blocked` do not request automatic continuation.
Missing or malformed blocks do not guess that more work is needed.

Automatic read-only fallback and continuation preserve each attempt in the same
run. The terminal transaction queues the successor before the run can appear
closed. Quota limits, cancellation, timeouts and unresolved custody retain their
existing recovery behaviour and do not trigger result-policy retries.
Allowances reset for each run, including a manually resumed run. The first
sealed retry decision survives interrupted settlement even if a scheduled or
expiring rule changes before recovery.
Result-check fallback never carries write authority. Capacity and account
recovery retain the task's existing worktree and write authority, and carry its
validated handoff rather than a transcript. Nested stores are lane-writable, so
restricted routes select their reliable fallback before launch there instead of
trusting a later retry record. Nested session continuations require explicit
caller recovery. Exact attempt inspection preserves original evidence;
run views, local Fabric waits and graph execution observe successors.
`run` keeps superseded receipts visible with `superseded_by` and derives its exit
status from the final successor. Waits include a successor queued during a poll.

`provenant telemetry stats` reports cache-read share by
model, account and lane using existing terminal usage categories. Lane JSON
also includes its share. The denominator is uncached input plus cache reads
plus cache writes. Unknown categories and empty denominators stay unknown.
Targets are advisory and configurable per policy; raise them against measured
work over time. The end-of-wave digest consumes this reporting surface in its
own delivery, #2189. Capture remains owned by #2174.
