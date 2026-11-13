# Slice B: soft writer budgets (#3257)

Implemented `wrap_up` (default), `warn` and `stop`, the 1,000,000-token default,
and 0.10 default grace. Budget, action and grace independently resolve run >
template > tier > project > host > default. Zero disables the target. Explanation
and status retain source metadata; status also reads the sealed overrun record.
Warn records the crossing and posts a bound-chair inbox notice without stopping
for tokens. Missing notice delivery is a guardian warning.

Wrap-up uses the existing RunStore same-session resume, admitted once before
predecessor custody releases and launched by the durable settlement reaction
once custody drops. Permanent resume refusals are recorded without retaining
predecessor custody; transient storage failures remain retryable. Its prompt carries the remaining grace and its persisted
profile makes that allowance a hard stop. The graph follows the run-internal
successor. Natural completion wins and skips budget snapshots; a clean effective
index plus a locally recorded pushed upstream stops at the boundary with the
lane's recorded handoff/outcome, without a timeout or snapshot. Git operations
in progress are not clean points.
Clean-point checks use a clean Git view with the effective private/native index.

Supporting files outside the owned list: records/run.rs and fabric-runtime/graph.rs
carry typed budget source metadata; records/attempt_file.rs classifies budget
evidence with the existing record-retention seam; run controls/reader/writer and CLI holder/launch
connect resume, overrun status, clean points and forwarded holder hooks;
src graph_interface and fabric interface/owner/settlement carry typed controls,
explanation and the ordinary resume launch. config/schema.rs exposes fractional
knobs. These are narrow prerequisites for the owned outcome. No other worktree,
shared branch, pull request, issue, or installed host config was changed.

Tests: added the action × clean point × natural completion invariant, independent
per-field layer/zero invariants, and a settlement regression for one same-session
continuation with remaining grace and no snapshot. Rewrote the three #3251
census/journal timing rows to test coalescing, eventual observation, spacing and
independent retry, rather than exact walk counts and timing samples. Rephrased
budget accounting test naming and preamble assertions to avoid hard-stop prose
coupling. No tests deleted.

Verification at implementation: cargo fmt and git diff --check. The sole local
cargo check -p provenant --tests found an omitted Kind::Fraction schema match arm;
fixed it and audited the other Kind matches. It was not rerun. Tests, clippy and
nextest were not run locally; pushed-branch CI owns compilation and execution.
The first lane CI (43418383505) found a collapsible-if lint, repaired alongside
the clean-boundary and refusal-release findings from focused source review.
The second lane CI (43419169966) found an unnecessary Option closure, also repaired.
Status limits now include live overrun/refusal evidence and distinguish the generated
wrap-up allowance from its original configured target. The clean-point regression
pushes native Git work to a local bare remote to verify real tracking evidence.
Integrated review/full CI remain the batch chair's gate. Current branch/CI evidence
is in the final schema-v2 handoff collected by the host.

Residual adapter: the existing RunStore::resume path and its predecessor/successor
records are deliberately reused for same-provider-session continuation. No new
provider adapter or fresh graph-restart mechanism was introduced. The integration
chair should reconcile this narrow bridge with the other identity cut-over slices.

Later CI evidence: lane run 43419732238 found the CLI launch failure needed
conversion at the settlement edge; repaired. Run 43420203940 passed workspace
compilation/lint, then strict reference generation found the compact key list
had grown beyond its existing surface policy. Kept the three daily budget knobs
in the compact list and left domain/tier/project variants in the advanced schema.
Both pure reference renderers were checked successfully without invoking Cargo.
Final exact-head CI evidence is recorded in the collected schema-v2 handoff.

The bounded same-session wrap-up defers context-threshold restart, preventing a
fresh restart from resetting the grace allowance. Its admission invariant checks
that the hard remaining allowance survives. The retry-extend preamble test also
uses resolved-value propagation rather than hard-stop wording.

The clean-point check covers both candidate and native indexes before the first
native commit, when equal heads cannot identify which index owns staged edits.
A regression stages a new file, removes it from disk, and keeps a pushed base;
it must remain dirty. This prevents a false clean stop that would skip wrap-up.
