# Migrate a batched lander
Owner: landing domain
Anchor: [`landing::driver::run`](../../tools/front-door/src/pr/landing/driver.rs); [`LandSettings`](../../tools/front-door/crates/cli/src/domains.rs)

A project with a bespoke batched lander and slow paid CI can configure a fast
self-hosted candidate check, opt-in PR CI and a full post-merge plan. First
arrange for the named fast check to run on candidate refs, and for the full check
to run on the target after merge. The bundled CI trigger covers `land/**`;
if `candidate_ref_prefix` changes, update the CI trigger to match.
`land --explain` warns about a non-default prefix. The following host `config.toml` example uses
illustrative check, label and pool names; replace them and the rates and limits
with the project's own values. Rates and limits use millionths of the configured
currency, and rates are per minute.

```toml
[domains.pr.land]
integration = "batch"

[domains.pr.land.gate]
kind = "checks"
checks = ["fast-self-hosted"]
coverage = ["fast"]
pool = "self_hosted"

[domains.pr.land.pr_ci]
mode = "opt_in"
label = "full-ci"

[domains.pr.land.post_merge]
checks = ["full-ci"]
pool = "hosted"
timeout_seconds = 3600
bisect = true
revert_forward = false

[domains.pr.land.budget]
advisory_micros = 5000000
cap_micros = 10000000

[domains.pr.land.budget.rate_micros_per_minute]
self_hosted = 1000
hosted = 100000
```

The opt-in label controls optional PR evidence, not forge-required checks. The
candidate gate still applies to every batch, including an unflagged single PR
with batch integration. `gate.pool` and `post_merge.pool` associate durations
with rates; a checks gate without a configured pool uses the check issuer app
slug. Unknown cost never refuses, and the cap refuses only priced spend.
Post-merge follow raises an `unsettled` alarm if checks remain pending at its
timeout. Choose a cap appropriate to the project's spending authority. See the
[settings reference](../reference/config-keys-all.md#host-land-settings) for the
remaining options and per-project and tier overrides.

Run the existing lander as the publisher while evaluating the same ordered PRs
with `provenant land 148 149 --shadow`. Inspect `provenant land --explain --json`
and the shadow receipt against the existing lander's record: compare base,
ordered approved heads, candidate tree, gate definition and result, selected
publication mode and reason, cost, per-PR outcomes and split decisions. A shadow run does not publish, and
its candidate can become stale if the existing lander advances the target;
compare runs made against the same base. Resolve discrepancies before switching
publication to Provenant. Retain the existing forge requirements throughout.
