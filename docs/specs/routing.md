# Routing, providers and quota

Owner: `crates/routing` (catalogue, resolve), `crates/providers` (adapters,
accounts, decoding), `crates/quota` (windows, refusals, readiness)
Anchor: `crates/routing/src/lib.rs`, `crates/providers/src/lib.rs`,
`crates/quota/src/admission.rs`, `crates/quota/src/readiness.rs`

How a dispatch becomes one exact provider model, and how quota and readiness
gate it. Policy and defaults are overridable data (`provenant config schema`);
`provenant route list` shows each entry's source.

## Exact selection, no inference

A request names one exact model or route. A shipped pool resolves its
model-family selectors against the effective catalogue; a user model pin stays
exact. The selected provider, model and effort are recorded before launch. An
adapter may declare that a valid but unsupported catalogued effort is dropped
before admission, and the route and launch record keep its warning and source;
invalid controls remain errors. A family comes only from provider protocol
evidence at the catalogue revision, never from a name or output text. A pool
draw is seeded from the operation, so a retry picks the same member without
stored state. Capability facts never grant filesystem or network authority, and
a confidential request admits only models with an explicit catalogue guarantee
that they do not train on prompts; unknown training treatment is excluded.

- Why: the receipt must name exactly what ran, and assurance claims must not be
  forgeable by output.

## Repository confidentiality at launch

The host classifies declared inputs and observed changes before route
selection. Protected inputs require no-training treatment for every candidate,
including fallback; incomplete evidence cannot silently declassify them, and
recovery keeps the accepted classification. Arbitrary later file access is not
classified.

- Why: confidential inputs must not reach a model that may train on them.

## One resolution, bounded alternatives

Resolution selects a route and bounded, ordered eligible alternatives from one
snapshot. An alternative equal to the primary model is dropped with a
`ROUTING_ALTERNATIVES` warning by default; strict policy refuses it. Malformed
choices, invalid efforts and duplicate fallback entries remain usage errors.
Preparation fallback follows [retry ownership](retry-owner.md).

- Why: the fallback plan is decided once and visible.

## Policy is overridable data

Bundled model policy, pools and writer advice are data that user, scope and
account rules overlay. A malformed restrictive rule refuses configuration and
never broadens; an optional preference warns and grants nothing. A model-choice
preference always expires.

- Why: changing operating policy should not need a Rust change, and a typo must
  not widen access.

## Provider model discovery

Provider CLI listings supply model availability and supported controls. A
successful refresh records a timestamped snapshot, a failed or overdue probe
keeps the previous one, and the effective catalogue overlays listings on
bundled metadata. A listing is not evidence of a model's confidentiality,
capability or answering family, and model-specific policy values stay with the
exact catalogue entry rather than passing to a new version.

Shipped pools and writer advice select the latest numeric version matching a
family selector, resolved independently so a family absent from a partial
listing keeps its bundled advice; an explicit user pool or model choice stays
pinned, and an uncatalogued explicit choice proceeds with a source warning. A
graph route family alias such as `claude/sonnet` resolves through the effective
catalogue once, at launch selection: the provider is launched with the exact
model and the launch receipt carries it, while the attempt's requested route
keeps the alias. An unknown or ambiguous alias warns.

For adapters that report per-account offered models, a catalogued
`superseded_by` edge withholds an older shipped pool member on an account that
offers a superseding model, and an unpinned selection never picks an account
whose known offer list lacks the model. `route resolve` reports the exclusion
with the recorded offers. With no observed offer list, shipped members stay and
supersedence is not applied; owner-configured pools stay pinned regardless. An
explicit `--account` pin is attempted with a warning rather than withheld.

`quota tick` refreshes listings for installed adapters with a registered
account and an enabled quota probe, concurrently within the tick's remaining
budget. Low-power ticks run no discovery tool, and `quota.model_discovery`
disables discovery globally.

## Provider model scope

Compiled provider model patterns constrain automatic choices from shipped
pools. Owner-configured pools, routes and preferences may select other models,
and an uncatalogued selection carries a source warning. When the owner sets
`providers.<adapter>.models`, that restriction applies to every automatic
choice: shipped and owner-configured pools, routes, preferences, fallbacks and
depletion hand-offs. A fallback model is an automatic alternative even when
attached to an explicit pin; it keeps its own readiness and is never promoted
by the advisory rule for exact pins. An explicit primary pin outside the scope
always launches: `out_of_scope_pin` chooses whether a warning naming the
setting's source is printed. Patterns are literal with `*` as a wildcard and
describe the requested model ID, not a model a provider's automatic mode picks.
`Catalogue::launch_model` applies a recorded effort suffix only to a catalogued
model that supports that effort, and the route and continuation identity keep the
canonical model and effort.

## Pinned review selection

`provenant run review-panel --pr N` (or a review leaf) pins the PR's base and
head on the host, independent of the launching checkout; supplying both
`subject` and `review_base` pins an explicit range instead. A repair re-check
adds `--set review_base=SHA` to `--pr N`: the head and PR binding still come
from the forge, while the diff narrows to `review_base...head`, matching the
three-dot range the host writes into its review context. `subject` stays
exclusive with `--pr`, since both would name the head and `--pr` takes it
from the forge. Diff evidence and selection use base...head, including branches that
merged main. Because the forge can report the previous head seconds after a
push, `--pr N --head SHA` pins the expected head: the host rereads the forge
within a bounded wait and refuses, naming both SHAs, if the mismatch persists.
Without `--head`, a local remote-tracking ref ahead of the forge warns and
names both commits. Every pinned launch receipt reports the base and head it
selected, and nested review launches keep the same pins. Without a subject the
host warns which checkout-derived range it chose and why.

A retry whose canonical raw-request identity matches a stored child receipt
replays that receipt and its admitted pins before host preparation, active
parent or not; the relay rechecks the identity under the journal lock so a
concurrent admission wins over newly prepared pins. An active launch without a
matching receipt resolves current pins, and an inactive parent cannot admit a
new child.

Before expanding the panel, the host derives changed paths and measured size
from the diff between the merge base and the subject and passes both to
`ReviewConfig::resolve_panel`. Configured path floors can raise the shape and
add lenses; explicit request fields and layered review configuration keep their
precedence. Bundled floors are repository-neutral, never select the deep shape
and warn on coverage shortfalls. A floor with `hard = true` refuses a shortfall.
Runtime confinement is independent of review floors. Empty-diff size metadata
proceeds with size unknown; incomplete path or size metadata applies the
configured unreadable-path floors and warns in reviewer guidance.

When a lane adds or edits a nested review panel, the relay uses the graph's
admitted review base and the source declared by that child or patch. An
integration source is known only after its immutable pin is recorded; its
admission-time branch base may be stale. A missing or still-moving head is
incomplete context. The relay never resolves refs again or substitutes the
current checkout head.

The host passes the brief to framing, every selected lens, verification and
synthesis; `provenant run review.security` invokes only that lens.

- Why: panel coverage must follow the change that reviewers will actually see,
  not the submitter's current checkout.

## Capability and selection preferences

Capability never excludes a listed pool member. Capability preferences rank
members alongside account load and outcome priors, and configuration can adjust
the weights. Exact model pins remain explicit choices.

Priors use attributed, comparable quality outcomes, with exploration among
eligible members; host failures are not quality observations, a validated
successful completed-before-budget result is one, and missing evidence stays
neutral. Producer and resolver share one prior-role derivation: an explicit role
wins, otherwise review workload is `review`, work with writes is `writer` and
other work is `task`. Candidate ranking and route diagnostics have typed owners.
Selection rules, exclusion sources, exploration and weights are typed receipt
fields; unknown forward fields alone use `ExtraFields`, and malformed known
fields fail decoding. Account ranking scales reset-adjusted usable headroom by
sourced penalties and divides by one plus live lanes.

Exhaustion is classified before weights, so a draw never offers a member
routing already judged spent and dispatch records no refuted pick for it. A
preview has no live probe: it uses its weighted draw, judges each member's
cached accounts, and the route it shows carries a redraw receipt naming
whichever member the draw offered first and that refusal's typed cause. Preview,
review-family and dispatch read one route-scoped exhaustion verdict: a snapshot
governs only the models its plan serves and only the windows bound to the
route's pools or allowance windows; the whole-snapshot reading applies only to a
route with no pool mapping. A spent paid window therefore cannot hide a
free-pool member the same account serves. Complete coverage still establishes
exhaustion when a window governing the route is spent; partial coverage remains
advisory and never refuses on its own.

- Why: capability is a preference, while attributed comparable outcomes supply
  the evidence that adjusts declared route weights.

## Provider configuration and local diagnostics

Provider configuration choices are host-sourced settings. Codex writers may
suppress inherited configuration through `ProviderSettings::clean_config`, and
read-only execution keeps clean configuration where extensions could exceed its
authority. The applied route records the choice and source without widening
credential or filesystem grants.

Unavailable adapter, model and effort capabilities may select an eligible
fallback, and invalid invocation syntax remains an operation error. Neither
establishes an authority boundary; account, route, continuation, explicit
read-only and pinned-source bindings still do.

Authorised task text reaches provider stdin or argv unchanged, including
secret-shaped prose and whitespace, and cloud handoff instructions append to the
brief. Local diagnostics keep content subject to formatting and length bounds,
and hook context is not silently rewritten. An advisory GitHub deny-list match
warns and forwards the exact invocation; host publication authority is separate.

- Why: preferences and capabilities are not hard boundaries, and authorised
  execution context must arrive intact (#3422).

## Capacity is not quota

Capacity (`model_unavailable`) and account allowance (`quota_limited`) are
separate outcomes. A capacity cooldown is an advisory selection preference that
explicit models ignore; only a quota-limited outcome cools an account. An
unclassified provider error settles as `Unclassified`, including after text and
exit 0, keeps the provider message and is never recorded as usage-limited; a
clean exit without an error still settles as `Succeeded`. Retries spend the
single lineage allowance in [retry owner](retry-owner.md).

- Why: capacity and quota need different remedies without multiplying retries.

## Unknown never means ready

Unknown or stale quota does not establish exhaustion, and missing, expired or
mismatched readiness evidence never reads as ready. Refusals are scoped to
model and account unless provider evidence proves a wider pool. A plan usage
limit holds the whole account until its reset, except that the exact model the
owner pinned warns with the record's source and expiry and is attempted anyway;
an automatic fallback or unpinned route still prefers an available route. That
advisory attempt covers a recorded exhaustion, pacing reserve or provider
refusal (including the free pool), and a model-only pin may hold the route for
one final attempt when its redraw to another account cannot serve the intent.
A missing login, confinement refusal or other non-advisory exclusion is never
answered with an advisory attempt.
OpenCode retry-after values are rounded up to whole seconds, and `quota show`
names the provider retry-after as the source of a displayed OpenCode refusal
reset.

A refusal expires at its provider reset, or one hour after observation when none
is usable; Agy's expire within five minutes even when the provider reports a
later reset, including when published readiness is read back, so a stale
multi-day estimate cannot block a new attempt or probe. Fresh provider evidence
may renew it, and a newer refusal for a pool supersedes its older reset. A fresh
reading below 95% headroom that postdates a refusal clears it; for a
model-scoped refusal without a pool every reported window must show headroom.
A successful run clears that model's refusal, and success on a free-model route
clears the registered free pool's refusal (`observed_success`). `quota tick`
rechecks old free-pool refusals with the adapter's registered probe, where one
exists; a refusal the probe does not supersede keeps its provider reset and gets
another bounded window. `quota clear` removes refusals on owner request and
reports exactly what it removed, saying so when nothing matched. Each clearing
records its source (`reading`, `run`, `observed_success` or `manual`), and
settlement replay respects the retained, bounded clearing records, so an evicted
clearing cannot prevent a later replay.

`Admission::account_block` decides account blocks for both resolution and the
pre-launch recheck. `Admission::route_exhausted` is the one route-scoped
allowance verdict preview, review-family and dispatch share. A typed
`AccountBlock` travels through admission, so an exclusion keeps its provider,
free-tier or mismatch cause while its text stays operator detail. Readiness is
stored apart from quota records; a failed publication leaves readiness unknown
and never erases held refusals. A pool
cannot draw a current login-required account even when every other member is
unavailable, and a missing availability observation reaches preparation for its
typed cause rather than launching an account. An observed account with unknown
identity follows the identity policy below. Sign-out and login-expiry
failures stay unavailable until a strictly newer successful login observation
or an explicit sign-in; for adapters with no login observation (OpenCode
included) they expire after the observation interval, and an executable-only
probe never establishes recovery. Identity mismatches still require identity
evidence. Preparation publishes its first typed login
failure, and a successful reprobe publishes recovery, through the quota and
readiness owner's account-scoped store. A pool exhausted by pre-launch
exclusions carries one typed aggregate preparation disposition even when the
reasons differ, so a declared fallback can advance.

A fresh probe failure (`command_failure` or `probe_timeout`) ranks its account
below ready or untested accounts for its validity window; if only failed-probe
accounts remain, launch proceeds with a warning unless the `PROBE_FAILURE`
policy refuses. A successful probe with figures records positive readiness, so
a completed tick never displays as unprobed.

- Why: one probe outage or model limit must not take a provider offline, and no
  stale evidence may admit a launch except for an exact model pin, whose
  advisory warning names the record's source and expiry before the attempt.
- Owner: `crates/quota/src/admission.rs`, `crates/quota/src/readiness.rs`,
  `AccountRecord::successful_login_observed_at` in `crates/quota/src/state.rs`,
  `crates/routing/src/resolve.rs`

## Monthly allowance ledger

`monthly_allowance_assessment` caps next-launch spend at the elapsed cycle
fraction plus burst, within model-pool scope. When cycles are unobserved it uses
available usage and a remembered or UTC calendar cycle estimate, including for
local logins, with a sourced policy warning rather than a hold; an estimate
never grants readiness. A confirmed reserve hold names the route, account
label, window and sourced setting in route resolution and refused-launch
summaries, and final reservation resolves labels and keys through the same
pacing configuration as route selection. An executable-only OpenCode observation
has no model offer list, so absent readiness warns and does not refuse an
otherwise eligible observed account. Unregistered opaque account keys stay
private.

`AllowanceLedger` retains costs across policy and cycle-estimate changes. When
discovery removes accounts, only canonical local-login keys keep debt and
registered records drop; ticks clear provider, probe-failure, login and
readiness observations, retrying cleanup later. Debt cannot supply review-family
candidates, and local preview requires provider or readiness evidence. First
samples baseline debt; deltas confirm only covered, pre-observation
reservations; flat or decreasing reports and incomplete journal coverage retain
debt; rollover preserves learned cost.

`reserve_allowance_launch` bypasses inapplicable policies. Contention admits
with warnings and durable receipts after bounded retries, and receipt
acknowledgements precede retirement so they survive crashes beyond journal
retention. `route list` exposes sourced settings, knobs, estimates and budgets,
and `pacing_preview` shares its assessment. [Pacing](../runbooks/routing-and-templates.md#pacing)
covers overrides and retention.

## Revisioned, bounded refresh

A quota refresh commits only against the revision it claimed, so stale
completions are discarded. Ordinary probes keep their tick deadline. A CLI
credential rotation has a separate admission budget and, once started, runs to
completion so its single-use refresh token persists; a short budget returns a
typed deferred result without starting it.

- Why: the menu bar ticks unattended and concurrently.

## Providers decide nothing

Providers do not publish completion or decide outcomes from prose. Structured
errors outrank claimed success, malformed frames fail closed, and continuation
keeps its selected identity and controls.

- Why: provider text must not decide an outcome or switch identity.

## Accounts and credentials

Account identity is detected, not guessed, and a changed identity around a probe
is refused. An availability record without an account key may proceed with a
warning; strict `QUOTA_IDENTITY_UNKNOWN` policy excludes it. Independent
credential, confinement and authority refusals still apply. A continuation keeps
the applied provider and optional account key as a pair: a missing provider, or a
kept key without an account for that provider, is refused, a same-key account
from another provider is never substituted, and an absent key stays absent. A
node-wide account pin does not switch accounts; each fallback candidate of an
unpinned node may declare its own.

Credential roots must not be HOME or an ancestor of HOME, and directories within
one account entry are aliases that may overlap. `identity::account_state_paths`
resolves each adapter's state and credential stores before registration,
selection and confinement compare them. HOME-rooted adapters own only their
declared client state below HOME (agy uses `.gemini`) and declared credential
files, not all of HOME. Nested HOME roots are allowed; overlapping resolved
stores, symlink aliases included, are refused by `accounts register` and
reported by `check config`. An adapter with no registered accounts that uses a
local login may show several quota entries with overlapping paths, because they
describe one login; this does not extend to isolated registered accounts. A login
at HOME needs no registration. Agy's local login stays selectable as
`account=local` beside registered accounts, with its own quota and readiness, as
an in-memory identity rather than a credential root; registering HOME explicitly
uses the detected identity and suppresses the synthetic entry so one login is not
routed twice. Native launches and account probes set HOME to the selected
account directory, and sandboxed launches keep a private HOME with only the
selected account's keychain. A registered long-lived lane token is not rotated
by the host. Credential custody belongs to
[supervision](supervisor.md#native-environment-and-sandbox-tool-stores).

OpenCode models are partitioned by the registered plan, independent of account
keys, labels and shared credential directories: `opencode-go/` models use the Go
plan and `opencode/` models use Zen. Enumeration keeps one row per registered
identity and never invents a Zen identity from Go credentials. The plan
declaration owns the quota source, so a missing or unrecognised plan keeps quota
unknown rather than using the Go ledger, and free-model pacing and refusals stay
scoped to the declared free pool.

- Why: misattribution can spend or expose the wrong account.

### Account references

Every command resolves an account reference through one
`providers::accounts::resolve` catalogue: a registry entry id, a label, an exact
account key, a harness-qualified `adapter/key` reference and a harness local
login all resolve to the same typed identity (adapter, key, entry id, label).
Each calling context declares the forms it accepts and projects that identity to
its own canonical output; no caller re-matches the reference. A reference that
matches more than one account is ambiguous in every context, with the matching
labels named as the repair. A miss lists close matching labels only — never
another account's directory, key or credential value; the setting path the
operator wrote is echoed.

## Launch route and role tiers

A role-specific route applies to matching graph nodes; other nodes keep their
template selection. Template defaults are data. `--route` takes one tier for
every node, a `role=tier` map, or an exact model as `ADAPTER/ID` — the same
value `run implement --model` accepts. An exact model lowers to an adapter and
model route, and the host entry point canonicalises its spelling once against
the effective catalogue, as it does for batch items; a tier name stays an
alias. The bundled smoke graph accepts the same values as any other template.

- Why: callers can choose a route by role without editing a template, and a
  model pin must not be mistaken for an alias by graph admission.

## Nested lanes

A lane's environment pins its depth, parent attempt and state directory. Inside
a lane Fabric refuses coordination mutations; graph submission and reads cross
the attempt-bound host relay.

- Why: a lane must not move claims or leases.

### The host validates and launches children

A child launch is journalled and launched by the host, bound to the calling
attempt rather than a lane-written record. Children share the graph's budget and
cancellation and may use another adapter and account within the user's
authority; ordinary launch policy still refuses state roots, while
credential-root overlap follows the resolved lane access. Parent settlement
closes attached children, never detached ones.

- Why: cross-family delegation must not make lane-controlled identity
  authoritative.
- Owner: `crates/cli/src/fabric/graph_relay.rs`, `crates/fabric-runtime/src/graph/relay.rs`

### Host-pinned Git

The host resolves and validates the Git executable pinned for lane use; a
replaced or loosened target is refused.

- Why: lane-chosen executables must not run with host authority.
