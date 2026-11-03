# Retry owner

Owner: run, routing, confinement and fabric-runtime
Anchor: `crates/run/src/policy.rs`

A retry lineage preserves each terminal receipt and its `retry_of` links. The
sealed policy decision carries a typed cause and action; absent fields mean
unknown and never authorise a writer retry. Terminal publication and successor
insertion are atomic. A stop decision is recorded even without a successor.
`Service::live_tail` resolves the live attempt with a bounded status and tail
recheck. Any unavoidable migration must be forward-only, idempotent and
preceded by a timestamped, WAL-aware store backup.

One cumulative infrastructure-successor allowance per lineage uses
`settlement_retries` (shipped default 2), overridden by preset then node
`retry.retries`; the effective value carries its origin. Graph and run must not
multiply the allowance. Observation and admission reprobes spend nothing.
Scoped exclusions carry cause, scope and expiry evidence. Account refusals
apply only at their evidenced scope. Exact pins, family separation, uncertain
launches and task failures are never redrawn. Cross-member writer fallback also
requires released custody, pinned HEAD, a clean worktree and no publication.

Admission keeps `--wait` as settlement observation timeout. `--admission-timeout`
defaults from the run admission policy. An admission window has one persisted absolute deadline and next wake under the same
operation key; restart and reprobe do not renew it or allocate an attempt.
Typed lock contention and scoped uncertainty may use bounded backoff; live
foreign writers, dirty trees and unknown ownership remain fail-closed. Census
uncertainty narrows only with evidence that a process cannot occupy the root.

Recovery uses resume, operation replay and unique continuations, with stable
identity per graph, node, terminal attempt and generation. Budgets survive
resumption; terminal receipts remain immutable. Only descendants blocked solely
by the recovered node reopen. Graph outcome is reported beside settledness.

There is no generic reason/action language, separate scheduler, duplicate
journal, counter or exclusion system, speculative crash restart, or long
automatic quota-reset wait. Partial fan-in minimums are separate work.

**Why:** A single owner and allowance make recovery traceable without expanding
writer authority or multiplying retries.

**Test:** `crates/run/src/policy.rs::tests` covers the implemented decision
and lineage contract; later budget, admission and recovery requirements are
review gates until their slices land.

**Owner:** run owns decisions and publication; routing owns budget and
exclusions; confinement owns census; fabric-runtime owns graph recovery.
