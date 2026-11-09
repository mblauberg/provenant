# Route readiness

Owner: `crates/quota` (record, freshness, publication), `crates/cli/src/quota.rs`
(persistence and the edge read)
Anchor: `crates/quota/src/readiness.rs`, `crates/cli/src/quota.rs`, `tests/quota.rs`, `tests/hosts_signin.rs`

Quota owns one readiness record per host, adapter, account and canonical model
or verified pool. It records a schema version, state (ready, unavailable or
unknown), typed reason, observation and expiry times, optional reset time and a
bounded non-secret evidence reference. Providers supply facts; quota publishes
verdicts. Selection gates for access, confinement, effort and pacing remain
contextual. Reason types are owned by `crates/quota/src/readiness.rs`.

Missing records, future observations, scope mismatches and records at or beyond
their expiry read as unknown. A reset does not by itself establish readiness.
A partial, uncalibrated usage window cannot establish headroom; a lower bound at
the limit still establishes exhaustion. Selection and admission exclude that
unknown headroom by default; `routing.allow_unknown_partial_headroom` permits
it without clearing other refusals. Account refusals cover all routes for
that account; pool refusals cover only the verified pool. Refusals publish as
soon as observed and retain the existing clearing and isolation rules.

Persist readiness separately from quota snapshots so older readers remain
unaffected. [State](state.md) owns the layout. An absent file reads as unknown; quota tick schedules a probe immediately for
an unprobed account, or after its configured panel interval from the last probe,
subject to power policy, backoff and an existing claim. A normally due probe
can run sooner.
Login-failure readiness expires at the quota probe interval; authentication
backoff does not extend it.
A changed agy fingerprint makes the old account unavailable and registers the
new identity.

Each account's readiness file also holds its latest non-secret login-expiry
observation. It is scoped to that host and account and uses the configured quota
polling interval as its freshness period. Missing, stale or future observations
and providers that do not expose expiry report unknown. A current expiry reports
fresh, expiring or expired relative to the clock and
`domains.quota.login_expiry_warning_seconds`. An expiring login warns without
excluding the route; a current, expired observation makes the account unavailable.
Neither state creates or clears quota refusals.

Scope. An observation's route is a canonical `adapter/model`, a verified pool,
or `*` for every route of the account. A pool refusal covers its pool only, a
refusal without a pool its model only, and an account-wide refusal all routes.
An account-wide ready observation does not establish model readiness.
Host is `local` for observations this machine publishes.

Writing. Quota derives provider-refusal observations from the account record
each time it commits that record: a refusal is published as `unavailable` with
the refusal's scope and reset, and removed when the account record no longer
holds it or when a fresh reading of the pool proves headroom
(`Admission::refusal_holds`, the rule admission uses). Forgetting the account
removes the file. A same-scope refusal replaces any prior observation. An
account-wide probe failure supersedes this host's per-route observations to
stay within the file bound; after probe success or expiry, the next account
sync rebuilds held refusals from the account record. A failed readiness write
leaves it unknown, never ready. The CLI warns if readiness publication fails;
the account commit remains authoritative.

Reading. Route lists and resolution show recorded model or verified-pool evidence
separately from executable availability; quota and doctor label account-wide
evidence as such. A route list caches each account file for the display and warns
when its 256-row readiness display limit is reached. Routing, route commands and
diagnostics read through the same scoped reader.
Doctor also runs declared local sign-in status commands for installed adapters
without account observations. Their fresh sign-in result is shown separately
from recorded readiness; an incomplete or inconclusive command is unknown.

Receipts. The receipt enum `RouteReadiness` (ready, unavailable) gains no
variant, because an older reader rejects an unrecognised status. Unknown
projects onto unavailable with the existing `unknown_availability` reason, or
`stale_availability` when the evidence expired (`RouteReadiness::unknown`,
`::stale`, `::is_unknown`). Observed reasons map to existing exclusion names:
missing executable, signed out to `auth_required`, expired login to
`login_expired`, account mismatch,
exhausted and provider refusal to `provider_refused`; the rest project as
unknown.

Dispatch may supersede older authentication evidence in memory after a fresh
account-scoped login probe. OpenCode has no isolated login-status probe: after
its executable check, dispatch warns that login is unverified and lets the
selected launch check it. This removes only older sign-out and expiry evidence
for that launch; it does not erase the stored observation, an account mismatch
or a provider refusal.

Routing, route commands, dispatch, host diagnostics, peer responses and quota
displays consume this record. Route previews do not run provider probes unless explicitly asked to probe. Receipt wire names remain compatible.
