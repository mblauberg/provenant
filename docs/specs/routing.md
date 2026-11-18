# Routing, providers and quota

Owner: `crates/routing` (catalogue, resolve), `crates/providers` (adapters,
accounts, decoding), `crates/quota` (windows, refusals, readiness)
Anchor: `crates/routing/src/lib.rs`, `crates/providers/src/lib.rs`,
`crates/quota/src/admission.rs`, `crates/quota/src/readiness.rs`

How a dispatch becomes one exact provider model, and how quota and readiness
gate it. Policy and defaults are overridable data
(`provenant config schema`). Paths are relative to
`tools/front-door/`.

## Exact selection, no inference

A request names one exact model or route. A shipped pool resolves its model-family
selectors against the effective catalogue; a user model pin stays exact. The
selected provider, model and effort are recorded before launch. An adapter may declare that a valid but unsupported
catalogued model effort is dropped before admission; the applied route and
launch record retain its warning and source. Invalid controls remain errors.
A family comes only from provider protocol evidence at
the catalogue revision, never from a name or output text. A pool draw is seeded
from the operation, so a retry picks the same member without stored state, and
the receipt names what ran and why. Capability facts never grant filesystem or
network authority, and a confidential request admits only models with an
explicit catalogue guarantee that they do not train on prompts. Unknown
training treatment is excluded.
- Why: the receipt must name exactly what ran, and assurance claims must not be
  forgeable by output.

## Repository confidentiality at launch

The host classifies declared inputs and observed changes before route selection.
Protected inputs require no-training treatment for every candidate, including
fallback; incomplete evidence cannot silently declassify them. Recovery retains
the accepted classification. This does not classify arbitrary later file access.

- Why: confidential inputs must not reach a model that may train on them.

## One resolution, bounded alternatives

Resolution selects a route and bounded, ordered eligible alternatives from one
snapshot. An alternative equal to the primary model is dropped with a
`ROUTING_ALTERNATIVES` warning by default; strict policy refuses it. Malformed
choices, invalid efforts and duplicate entries within the fallback list remain
usage errors. Preparation fallback follows [retry ownership](retry-owner.md).

- Why: the fallback plan is decided once and visible.

## Policy is overridable data

Bundled model policy, pools and writer advice are data that user, scope and
account rules overlay. A malformed restrictive rule refuses configuration and
never broadens; an optional preference warns and grants nothing. A model-choice
preference always expires.
- Why: changing operating policy should not need a Rust change, and a typo must
  not widen access.

## Provider model discovery

Provider CLI listings supply model availability and supported controls. A successful
refresh records a timestamped snapshot; a failed probe keeps the preceding
snapshot. The effective catalogue overlays those listings on bundled metadata.
A listing is not evidence of a model's confidentiality, capability, or answering
family. Shipped pools and writer advice select the latest numeric version matching
a family selector; an explicit user pool or model choice stays pinned. Model-specific
policy values stay with the exact catalogue entry rather than being inferred for a
new version. Explicit uncatalogued choices may proceed with a source warning.
`quota tick` refreshes listings and reports changes for installed adapters with a
registered account and an enabled quota probe. Low-power ticks invoke no discovery
tools; `knobs.quota.model_discovery` disables discovery globally (default true).
Probes run concurrently within the quota tick's remaining wall budget, with one
second reserved for reporting. Failed or overdue probes retain their previous
snapshots. Advisory family selectors resolve independently, retaining bundled advice
for a family absent from a partial listing. Discovery messages are transient command
output, not part of the persisted quota report. Route listings name the source of
model, valid pair, named route and pool entries in human and JSON output.

## Provider model scope

Compiled provider model patterns constrain automatic choices from shipped
pools. Owner-configured pools, routes and model preferences may select other
models; an uncatalogued selection carries a source warning. When the owner explicitly sets
`providers.<adapter>.models`, that restriction applies to automatic choices
from both shipped and owner-configured pools, routes, preferences, fallbacks and
depletion hand-offs. A fallback model is an automatic alternative, even when
attached to an explicit model pin. `out_of_scope_pin` applies to an explicit
primary model choice; it does not filter fallback alternatives. An explicit
primary pin outside the effective scope follows the provider's policy: warn
once and launch, refuse, or allow silently. Provider model names are matched
against literal patterns with `*` as a wildcard. Scope describes the requested
model ID, not an underlying model selected by a provider's automatic mode.
`Catalogue::launch_model` applies a recorded effort suffix only to a catalogued
model with that supported effort; Cursor receives that spelling at its argv
boundary while route and continuation identity retain the canonical model and
effort.

## Pinned review selection

When `provenant run review-panel` or a review leaf supplies both `subject` and
`review_base`, the host resolves them to immutable commits once. Before
expanding the full panel, it
derives changed paths and measured size from the merge-base-to-subject diff and
passes both to `ReviewConfig::resolve_panel`. Configured path floors can raise
the selected shape and add their lenses; explicit request fields and layered
review configuration keep their existing precedence. Bundled floors are repository-neutral, do not select the deep shape, and warn on coverage shortfalls. A project may declare its own floors under `[domains.routing.review.floors]` in `.provenant/config.toml`; only a floor with `hard = true` refuses a coverage shortfall. Runtime confinement is independent of review floors. If size metadata is
unavailable because the diff is empty, selection proceeds with size unknown. If
path or size metadata is incomplete, the resolver applies the configured
unreadable-path floors and adds a warning to reviewer guidance.

When a lane later adds or edits a nested review panel, the relay uses the
graph's admitted review base and the source declared by that child or patch.
Explicit commit sources can differ from the calling node's source. Settled
node outputs can supply their recorded commit. An integration source is known
only after its immutable integration pin is recorded; its admission-time branch
base alone may be stale. A missing or still-moving head is incomplete context
and applies the configured unreadable-path floors, alongside paths captured
from any resolved subjects. The relay does not resolve refs again or substitute
the current checkout head.

Use this command for the complete policy-selected panel:

```sh
provenant run review-panel --brief-file brief.md --set subject=origin/branch --set review_base=origin/main
```

The host passes the brief to framing, every selected lens, verification and
synthesis. A command such as `provenant run review.security` invokes only that
lens.

- Why: panel coverage must follow the change that reviewers will actually see,
  not the submitter's current checkout.

## Capability and selection preferences

Capability never excludes a listed pool member. Capability preferences rank
members alongside account load and outcome priors; configuration can adjust
those weights. Exact model pins remain explicit choices.

Account load and outcome priors rank eligible members. Priors use attributed,
comparable quality outcomes, with exploration among eligible members;
host failures are not quality observations, while a validated successful
completed-before-budget result remains one. Missing outcome evidence stays
neutral. The producer and resolver share one prior-role derivation: an explicit
role wins, otherwise review workload uses `review`, work with writes uses
`writer`, and other work uses `task`. Attempts retain typed workload and write
facts so role-less work contributes comparable priors.

Selection rules, exclusion sources, exploration and weights are typed receipt
fields. Candidate ranking and route diagnostics have typed owners; unknown
forward fields alone use `ExtraFields`. Malformed known fields fail decoding.
Account ranking scales reset-adjusted usable headroom by sourced
penalties and divides by one plus live lanes.

- Why: capability is a preference, while attributed comparable outcomes supply the evidence that adjusts declared route weights.

## Capacity is not quota

Capacity (`model_unavailable`) and account allowance (`quota_limited`) are
separate outcomes. A capacity cooldown is an advisory selection preference that
explicit models ignore; only a quota-limited outcome cools an account. Retries
spend the single lineage allowance in [retry owner](retry-owner.md).
- Why: capacity and quota need different remedies without multiplying retries.

## Unknown never means ready

Unknown or stale quota does not establish exhaustion. Monthly allowance pacing
constrains unobserved cycles as described below. Missing, expired or
mismatched readiness evidence never reads as ready. Refusals are scoped to
model and account unless provider evidence proves a wider pool; a plan usage
limit holds the whole account until its reset. Agy's recorded refusals expire
within five minutes even when the provider reports a later reset. This bound
also applies when reading previously published readiness, so a stale multi-day
estimate cannot prevent a new attempt or probe. Fresh provider evidence may
renew the refusal. One function,
`Admission::account_block`, decides account blocks for both resolution and
the pre-launch recheck. Readiness is stored apart from quota records; a failed
publication leaves readiness unknown and never erases held refusals.
A fresh probe failure (`command_failure` or `probe_timeout`) ranks its account
below ready or untested accounts during its validity window; if only failed-probe
accounts remain, launch proceeds with a warning unless the `PROBE_FAILURE`
policy knob refuses. Readiness display distinguishes "not probed" (with a tick hint)
from "probed, provider reports no figures" and "probed, failed" (both without hint).
Unknown provider errors whose kind and status are absent or unknown settle as
`unclassified`, not `usage_limited`: they preserve the provider message, retry once
on the same route without marking the account exhausted, then reroute at most
once to another eligible route or provider at the same tier before stopping at
the chair.
- Why: one probe outage or model limit must not take a provider offline, and
  no stale evidence may admit a launch.
- Owner: `crates/quota/src/admission.rs`, `crates/quota/src/readiness.rs`,
  `crates/routing/src/resolve.rs`

## Monthly allowance ledger

`monthly_allowance_assessment` caps next-launch spend at elapsed cycle fraction
plus burst, respecting model-pool scope. Missing registered cycles constrain
eligibility; local logins roll remembered cycles forward or estimate UTC months,
exposing the source without granting readiness.

`AllowanceLedger` retains costs across policy and cycle-estimate changes.
When discovery removes accounts, only canonical local-login keys retain debt;
registered records drop. Ticks clear provider, probe-failure, login
and readiness observations, retrying cleanup later. Debt cannot supply
review-family candidates; local preview requires provider or readiness evidence.
First samples baseline debt; deltas confirm only covered, pre-observation
reservations. Flat/decreasing reports and incomplete journal coverage retain
debt. Rollover preserves learned cost.

`reserve_allowance_launch` bypasses inapplicable policies. Contention admits with
warnings and durable receipts after bounded retries. Receipt acknowledgements
precede retirement, surviving crashes beyond journal retention.

Canonical local logins accept account overrides. `route list` exposes sourced
settings, knobs, estimates and budgets; `pacing_preview` shares its assessment.
[Operating details](../runbooks/routing-and-templates.md#pacing) cover overrides
and retention.

## Revisioned, bounded refresh

A quota refresh commits only against the revision it claimed, so stale
completions are discarded. Ordinary probes keep their tick deadline. A CLI
credential rotation has a separate admission budget and, once started, runs to
completion so its single-use refresh token can be persisted safely. A short
budget returns a typed deferred result without starting the refresh.
- Why: the menu bar ticks unattended and concurrently.

## Providers decide nothing

Providers do not publish completion or decide outcomes from prose. Structured
errors outrank claimed success, malformed frames fail closed, and continuation
preserves its selected identity and controls.

- Why: provider text must not decide an outcome or switch identity.

## Accounts and credentials

Account identity is detected, not guessed; a changed identity around a probe is
refused. An availability record without an account key may proceed with a
warning by default; strict `QUOTA_IDENTITY_UNKNOWN` policy excludes it.
Independent credential, confinement and authority refusals still apply.
A continuation retains the applied provider and optional account key
as a pair. A missing provider, or a retained key without an account for that
provider, is refused; a same-key account from another provider is never
substituted. An absent applied account key remains absent. Credential roots
must not be HOME or an ancestor of HOME.
Additional directories within one account entry are aliases and may overlap.
`identity::account_state_paths` resolves each adapter's state and credential
stores before registration, registry selection and confinement compare them.
HOME-rooted adapters own their declared client state below HOME (agy uses
`.gemini`) and their declared credential files; their entire HOME is not a
credential store. Nested HOME roots are allowed, while overlapping resolved
stores, including symlink aliases, are refused by `accounts register` and
reported by `check config`. An adapter
that declares no registered accounts but uses a local login can have multiple
quota/menu entries whose directory paths overlap; those entries all describe
the same local login, not separate credential trees. This allowance is limited
to that adapter and does not include isolated registered accounts. A login at
HOME needs no registration. Agy's local login remains selectable as
`account=local` beside isolated registered accounts and participates in pools
with its own quota and readiness. It is an in-memory identity rather than a
registered credential root. Registering HOME explicitly uses its detected
identity and suppresses the synthetic local entry to avoid routing the same
login twice. Native launches and account probes set HOME to the selected
account directory; sandboxed launches retain a private HOME with only the
selected account's keychain. A pin does not switch accounts. Credential custody
is owned by [supervision](supervisor.md#native-environment-and-sandbox-tool-stores).
A registered long-lived lane token is not rotated by the host.

OpenCode models are partitioned by the registered plan, independently of account
keys and labels. Enumeration keeps one row per registered identity and never
invents a Zen identity from Go credentials. Plan declarations also own the quota
source: missing or unrecognised plans retain unknown quota evidence rather than
using the Go ledger. Free-model pacing and refusals remain
scoped to the declared free pool.

- Why: misattribution can spend or expose the wrong account.

## Launch route and role tiers

A role-specific route applies to matching graph nodes; other nodes retain their
template selection. Template defaults are data.

- Why: callers can choose a route by role without editing a template.

## Nested lanes

A lane's environment pins its depth, parent attempt and state directory. Inside
a lane Fabric refuses coordination mutations; graph submission and reads cross
the attempt-bound host relay.
- Why: a lane must not move claims or leases.

### The host validates and launches children

A child launch is journalled and launched by the host, bound to the calling
attempt rather than a lane-written record. Children share the graph's budget
and cancellation and may use another adapter and account within the user's
authority; ordinary launch policy still refuses credential and state roots.
Parent settlement closes attached children, never detached ones.
- Why: cross-family delegation must not make lane-controlled identity
  authoritative.
- Owner: `crates/cli/src/fabric/graph_relay.rs`, `crates/fabric-runtime/src/graph/relay.rs`

### Host-pinned Git

The host resolves and validates the Git executable pinned for lane use; a
replaced or loosened target is refused.

- Why: lane-chosen executables must not run with host authority.
