# Route readiness

Owner: `crates/quota` (evidence and freshness), `crates/cli/src/quota.rs`
(persistence and publication)
Anchor: `crates/quota/src/readiness.rs`, `tests/quota.rs`, `tests/hosts_signin.rs`

Quota publishes readiness from provider facts, scoped to host, adapter, account
and canonical model or verified pool. Recorded readiness is separate from
contextual access, confinement, effort and pacing eligibility. Types and reasons
are owned by `crates/quota/src/readiness.rs`; receipt projection is owned by
`crates/records/src/resolved_route.rs::RouteReadiness`.

## Unknown never means ready

Missing, future, expired or mismatched evidence reads as unknown. A quota reset
alone cannot establish readiness. Account-wide readiness cannot prove model
readiness; account refusals cover every route, while pool refusals cover only
the verified pool. Partial uncalibrated headroom is unknown and excluded unless
the user enables the documented exception; an exhausted lower bound still
excludes it.
- Test: `crates/quota/src/readiness.rs::tests::readiness_ready_evidence_requires_fresh_matching_scope`;
  `crates/quota/src/readiness.rs::tests::refusals_publish_by_scope_and_clear_with_the_account_record`

## Publication preserves the account record

Readiness is persisted separately from quota snapshots; [state](state.md) owns
layout. A failed publication warns and leaves readiness unknown, never ready;
the committed account record remains authoritative. A fresh observation clears
only the refusal its scope and evidence disprove. A probe failure cannot erase
held route refusals when normal publication resumes.

Login expiry uses fresh account-scoped evidence. An expiring login warns; a
current expired login excludes. Neither creates nor clears quota refusals.
Authentication backoff does not extend readiness freshness.
- Test: `crates/quota/src/readiness.rs::tests::full_refusal_file_accepts_probe_failure_and_restores_refusals`;
  `crates/quota/src/readiness.rs::tests::fresh_expiry_excludes_only_after_expiration_and_expires_with_evidence`;
  `crates/quota/src/readiness.rs::tests::login_failure_expires_at_the_probe_interval`

## Readers keep evidence and execution distinct

Route resolution, diagnostics and peer reports use the same scoped evidence.
Previews do not probe providers unless explicitly requested. A fresh host login
probe may supersede older authentication evidence for that launch, without
erasing persisted account mismatch or provider refusal. An adapter without an
isolated status probe reports login as unverified and lets launch establish it.

Unknown readiness projects to unavailable with an unknown reason; expired
evidence projects with a stale reason. Typed observed refusals map to the receipt
enum, and other reasons remain unknown. Displays must distinguish account-wide
observations from model or verified-pool evidence.
- Test: `crates/quota/src/readiness.rs::tests::receipt_projection_covers_every_reason_and_ready`
