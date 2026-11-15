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

A request names one exact model or route. No adapter, upgrade, effort or
downgrade is inferred. An adapter may declare that a valid but unsupported
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
snapshot. Preparation fallback follows [retry ownership](retry-owner.md).

- Why: the fallback plan is decided once and visible.

## Policy is overridable data

Bundled model policy, pools and writer advice are data that user, scope and
account rules overlay. A malformed restrictive rule refuses configuration and
never broadens; an optional preference warns and grants nothing. A model-choice
preference always expires.
- Why: changing operating policy should not need a Rust change, and a typo must
  not widen access.

## Provider model scope

Provider model patterns constrain automatic pool draws, named routes and their
fallbacks. An explicit pin outside the scope follows the provider's configured
`out_of_scope_pin` policy: warn once and launch, refuse, or allow silently.
Provider model names are matched against literal patterns with `*` as a wildcard.
Scope describes the requested model ID, not an underlying model selected by a
provider's automatic mode. `Catalogue::launch_model` applies a recorded effort
suffix only to a catalogued model with that supported effort; receipts retain the
canonical model and effort.

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

Unknown or stale quota never causes a refusal, and missing, expired or
mismatched readiness evidence never reads as ready. Refusals are scoped to
model and account unless provider evidence proves a wider pool; a plan usage
limit holds the whole account until its reset. One function,
`Admission::account_block`, decides account blocks for both resolution and
the pre-launch recheck. Readiness is stored apart from quota records; a failed
publication leaves readiness unknown and never erases held refusals.
- Why: one probe outage or model limit must not take a provider offline, and
  no stale evidence may admit a launch.
- Owner: `crates/quota/src/admission.rs`, `crates/quota/src/readiness.rs`

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
refused. Registered account directories must not contain HOME or overlap each
other; `accounts register` refuses these and `check config` reports them. A
login at HOME is the adapter's local account and needs no registration. A pin does not switch accounts. Credential custody is owned by
[supervision](supervisor.md#native-environment-and-sandbox-tool-stores). A registered long-lived lane token is not rotated by the host.

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
