# Retry owner

Owner: run, routing, confinement and fabric-runtime
Anchor: `crates/run/src/dispatch_temporary_tests.rs`

A retry lineage preserves each terminal receipt and its `retry_of` links. The
version 2 sealed policy decision requires a typed cause and action. Drain
active runs before upgrading; prior decision records are not read. Terminal
publication and successor insertion are atomic. A stop decision is recorded even without a successor.
Any unavoidable migration must be forward-only, idempotent and
preceded by a timestamped, WAL-aware store backup.

One cumulative infrastructure-successor allowance per lineage uses
`settlement_retries` (shipped default 2), overridden by preset then node
`retry.retries`; the effective value carries its origin. Graph and run must not
multiply the allowance. Observation, admission reprobes and verified pre-launch
preparation fallbacks spend no settlement retries. Scoped account refusals apply
only at their evidenced scope. Route switches require a verified local
preparation failure and released custody; launched or uncertain attempts do not
switch routes. A quota-stopped writer may continue on another eligible account
with an unfinished handoff.

Admission keeps `--wait` as settlement observation timeout. `--admission-timeout`
defaults from the run admission policy. A shorter `--wait` shortens the window to
no less than 60 seconds, so a short observation does not refuse a lane over lock
contention. An admission window has one persisted absolute deadline and next wake under the same
operation key; restart and reprobe do not renew it or allocate an attempt.
Typed lock contention and scoped uncertainty may use bounded backoff; live
foreign writers, dirty trees and unknown ownership remain fail-closed. Census
uncertainty narrows only with evidence that a process cannot occupy the root.

Recovery uses resume, operation replay and unique continuations, with stable
identity per graph, node, terminal attempt and generation. Budgets survive
resumption; terminal receipts remain immutable. Only descendants blocked solely
by the recovered node reopen. Graph outcome is reported beside settledness.

A same-node writer policy continuation pins to its predecessor's settled head
when that head descends from the node source. `source_head` retains the original
pin even when the predecessor did not move HEAD; the lens diff base stays the
node source. Preparation reports uncommitted predecessor paths and preserves
their edits. A failed Git status refuses preparation.

There is no generic reason/action language, separate scheduler, duplicate
journal, counter or exclusion system, speculative crash restart, or long
automatic quota-reset wait. Partial fan-in minimums are separate work.

**Why:** A single owner and allowance make recovery traceable without expanding
writer authority or multiplying retries.

**Test:** `crates/run/src/dispatch_temporary_tests.rs::fresh_resume_preserves_lineage_and_authority_without_provider_state` covers continuation lineage; other requirements are review gates where no test pins them.

The run continuation driver and graph worktree driver cover real successor
custody, launch and guardian settlement. Native confinement is required for
those launch checks; unavailable local backends report a skip.

**Owner:** run owns decisions and publication; routing owns budget and
exclusions; confinement owns census; fabric-runtime owns graph recovery.
