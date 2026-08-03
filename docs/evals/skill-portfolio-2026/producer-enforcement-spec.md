# Producer-Enforcement Cluster Specification

## 1. Issue map

`gh issue list --limit 200` was attempted but GitHub API access failed in the local shell. The connected repository issue reader was used as the read-only equivalent, and each implicated issue was read individually. The verified count is eleven.

| # | Issue | Obligation | Cluster result |
|---:|---|---|---|
| 546 | [Lane-authored delivery receipts fail validation on first contact](https://github.com/mblauberg/provenant/issues/546) | Provide a harness-owned receipt scaffold that records real timestamps, executed evidence, digests and review lineage. | Partial. This cluster prevents review self-attestation, but does not build the delivery receipt scaffold. |
| 550 | [Add delivery receipt producer as sole RUN.json writer](https://github.com/mblauberg/provenant/issues/550) | Make the receipt producer the only writer of `RUN.json`, with atomic mutation and producer-side refusals. | Not closed. Delivery receipt ownership is outside this cluster. |
| 551 | [Producer-generated fixtures](https://github.com/mblauberg/provenant/issues/551) | Generate validator fixtures through the real receipt producer and preserve adversarial validator tests. | Not closed. This cluster does not change delivery fixtures. |
| 552 | [Delete RUN.template.json and rewrite deliver workflow](https://github.com/mblauberg/provenant/issues/552) | Remove the freehand receipt path and require all receipt creation through the producer. | Partial. Prompt enforcement is strengthened, but the delivery template and workflow remain outside scope. |
| 574 | [Delivery receipt validator parity](https://github.com/mblauberg/provenant/issues/574) | Mirror producer-side path, digest, route eligibility and timestamp invariants in the validator. | Partial. The review-route subcase is reinforced, but delivery validator parity and module splitting are not addressed. |
| 579 | [Workers report nonexistent commit SHAs](https://github.com/mblauberg/provenant/issues/579) | Verify claimed commits with host-side Git evidence or make the wrapper own Git operations. | Partial. The same producer/checker rule applies to review evidence, but commit existence verification is not added here. |
| 582 | [Lanes report scoped tests as the full gate](https://github.com/mblauberg/provenant/issues/582) | Bind gate claims to the exact command, collection counts, pass counts and captured output. | Partial. The cluster blocks self-reported review lineage, but does not add execution-count evidence. |
| 584 | [Chair-built receipts bypass the producer](https://github.com/mblauberg/provenant/issues/584) | Route chair-built receipts through the canonical producer or give the validator equivalent coverage. | Not closed. The delivery receipt path is outside this cluster. |
| 587 | [Evidence schema cannot falsify its author's claim](https://github.com/mblauberg/provenant/issues/587) | Ensure evidence contains at least one independently observed fact that can contradict its author. | Partial. Review provenance becomes independently sourced, but delivery evidence remains a separate concern. |
| 590 | [Chair-created load causes invented failure mechanisms](https://github.com/mblauberg/provenant/issues/590) | Record concurrency assumptions, observed failure data and baseline measurements before assigning an environmental cause. | Partial. Self-reported review provenance is guarded, but concurrency and failure-mechanism evidence are not implemented. |
| 598 | [Review leg can terminate without a visible verdict](https://github.com/mblauberg/provenant/issues/598) | Require an explicit terminal outcome, preserve unavailable legs, capture exit status and prohibit wrapper reconstruction. | Partial. The no-reconstruction rule is enforced for this collision class; detached exit capture and full terminal semantics remain separate work. |

The cluster therefore closes specific producer-collision obligations in issues 579, 582, 587 and 598, but does not close any of the eleven issues in their entirety.

## 2. Site enumeration

The enumeration is divided into production prompt sites, runtime joins, adjacent producer/checker boundaries and test fixtures.

| ID | Site | Current collision or boundary |
|---|---|---|
| P1 | `$AGENTS_HOME/.worktrees/baseline-integration/workflows/implement-run.js:281-300, 586-596` | Review prompts expose dispatcher-owned route fields and, at current HEAD, instruct the reviewer to copy normalised dispatcher fields into its structured return. |
| P2 | `$AGENTS_HOME/.worktrees/baseline-integration/workflows/codebase-polish.js:408-430` | The cross-family reviewer is instructed to read dispatcher fields, return the dispatcher-known `reviewerId`, set the dispatcher-known record path and return a structured cross-family record. |
| P3 | `$AGENTS_HOME/.worktrees/baseline-integration/workflows/cross-verify.js:266-313` | The cross-family prompt says the worker returns normalised guarantees verbatim, captures the dispatcher record and returns fields derived from it. |
| P4 | `$AGENTS_HOME/.worktrees/baseline-integration/skills/orchestrate/references/orchestration-contract.md:82-92` | The worker contract requires every worker to state route tier, model identity, route receipt and identity, although those are dispatcher-known facts. |
| P5 | `$AGENTS_HOME/.worktrees/baseline-integration/skills/orchestrate/references/worker-liveness.md:34-36, 81-86` | Safe adjacent doctrine. It explicitly requires the dispatcher to return observed terminal facts and prohibits invention. |
| P6 | `$AGENTS_HOME/.worktrees/baseline-integration/skills/orchestrate/references/cli-headless.md:198-215` | Safe dispatcher producer. It defines the normalised route record and distinguishes the human answer, dispatcher terminal and semantic worker result. |
| R1 | `$AGENTS_HOME/.worktrees/baseline-integration/skills/orchestrate/scripts/cf_dispatch.sh:195-241, 507-565` | Host-side dispatcher produces route and terminal records from observed process status, output and digests. The producer is distinct from the model response. |
| R2 | `$AGENTS_HOME/.worktrees/baseline-integration/skills/_shared/worker_outcome.py:241-363` | Host-side outcome acceptance checks route identity, terminality, digests, distinct paths and distinct inodes. This file is excluded and remains unchanged. |
| R3 | `$AGENTS_HOME/.worktrees/baseline-integration/skills/orchestrate/scripts/run_dir_finalize.py:151-193` | The finaliser joins dispatcher route data, semantic terminal data and `review["verdict"]`, then passes both verdict inputs to the existing gate. This is the principal enforcement site. |
| R4 | `$AGENTS_HOME/.worktrees/baseline-integration/skills/orchestrate/scripts/run_dir_finalize.py:303-399` | Route identity is compared with review-plan declarations. The route artifact must become authoritative for actual provider and dispatch provenance. |
| R5 | `$AGENTS_HOME/.worktrees/baseline-integration/skills/_shared/review_terminal.py:88-175` | The existing gate compares worker and wrapper verdicts and genuinely fails on mismatch. This file is excluded and remains unchanged. |
| D1 | `$AGENTS_HOME/.worktrees/baseline-integration/skills/deliver/scripts/delivery_validation_evidence.py:124-230` | Delivery evidence compares declared status with observed exit status and captured execution data. This is an adjacent producer/checker issue, not a review-topology site. |
| D2 | `$AGENTS_HOME/.worktrees/baseline-integration/skills/deliver/scripts/delivery_validation_reviews.py:20-113` | Delivery review lineage is compared with a bound route receipt and judgement evidence. The values can still originate from one receipt-producing path. |
| D3 | `$AGENTS_HOME/.worktrees/baseline-integration/skills/evaluate/scripts/validate_evaluation.py:930-999` | Evaluation validation checks that graders and adjudicators are independent by provider family, session and route receipt. This site already has a producer distinction. |
| D4 | `$AGENTS_HOME/.worktrees/baseline-integration/skills/implement/scripts/accept_claim.py:66-118` | The host invokes an installed verifier and checks its observed result against the claimed worktree and commit. This is already producer-distinct. |
| D5 | `$AGENTS_HOME/.worktrees/baseline-integration/skills/implement/scripts/bind_merged_delivery.py:118-176` | Host-side GitHub and Git observations are checked against the receipt and review artefacts. This is already producer-distinct. |
| T1 | `$AGENTS_HOME/.worktrees/baseline-integration/tests/test_run_dir_finalize.py:39-137` | The fixture creates evidence, semantic result, answer, dispatcher terminal and route receipt in one helper. It can accidentally model the same producer on both sides. |
| T2 | `$AGENTS_HOME/.worktrees/baseline-integration/skills/orchestrate/evals/test_cf_dispatch.py:403-472` | The direct-dispatch fixture binds dispatcher output, semantic worker result and finaliser input. It is the correct place to preserve the independent producer topology in tests. |

Prompt and instruction text was searched across `skills/`, `workflows/`, and template-like content. No additional production prompt asks a worker to return dispatcher-owned provenance beyond P1, P2, P3 and P4. P5 and P6 are explicit safe patterns and must remain allowed.

## 3. End-state table

| ID | Intended behaviour | Producer identity fact | Failure behaviour |
|---|---|---|---|
| P1 | The repaired workflow asks the reviewer only for review content and semantic verdict. Dispatcher route fields are read by the host and never returned by the reviewer. | Dispatcher identity is the bound route receipt `(attempt_id, route digest)`. Reviewer identity is the host-created review row identity. | Static producer scan fails with `PE001`; the harness gate stops before tests or execution. |
| P2 | The reviewer may map the external answer to a verdict and anchors, but must not return `reviewerId`, `recordPath` or copied route guarantees as dispatcher facts. | Route path and reviewer ID come from the dispatcher invocation and bound route receipt. | Static scan fails with the source line and forbidden field. No compatibility interpretation is provided. |
| P3 | The cross-family driver returns only its semantic verdict, anchors and explicit unavailable reason. The dispatcher record remains a separate host artefact. | Dispatcher terminal and route are host-produced. Semantic verdict and anchors are worker-produced. | Static scan fails for any verbatim or copied dispatcher guarantee. Finalisation also fails if the semantic result contains dispatcher-owned fields. |
| P4 | The worker contract describes assigned inputs and expected outputs but does not require workers to attest to route or model facts already owned by the dispatcher. | Route, model and provider identity are obtained from the route receipt, not from worker prose. | Static scan fails the contract text. |
| P5 | Retain the existing rule that a dispatching worker returns the launcher’s observed terminal result and never reconstructs the provider’s work. | Native launcher observation is the producer; the dispatching worker is only a transport. | Existing liveness and outcome failure remains explicit. No fallback reconstruction is permitted. |
| P6 | Retain the dispatcher record shape and the separate human answer, dispatcher terminal and semantic result. | `cf_dispatch.sh` owns route and terminal production. | Existing route or digest validation fails closed. |
| R1 | Continue producing route records from observed process state. Do not accept model-returned route fields as authoritative. | `dispatcher-record` identity is `(attempt_id, route receipt digest)`. | Missing, inconsistent or non-observed route data produces a non-passing route record. |
| R2 | Leave the existing distinct-path, distinct-inode and terminal checks unchanged. | `dispatcher-record`, `dispatcher-terminal`, `human-answer` and `worker-semantic` are separate artefact roles. | Existing `accept_worker_outcome` rejection is propagated by the finaliser. |
| R3 | Call the existing verdict gate only after the producer boundary has been checked. Read semantic verdict from the bound terminal result and route provenance from the bound route receipt. Never infer route provenance from `review["verdict"]` or worker output. | Semantic result is `worker-semantic`; route receipt is `dispatcher-record`; finaliser is `host-consumer`. | Return `worker outcome rejected: producer-enforcement` and leave the run non-terminal or failed according to existing finaliser policy. |
| R4 | Treat route receipt values as the authoritative actual lineage. Review-plan lineage is only a declaration that must agree with the route. | Actual adapter, provider family, model family, endpoint, attempt and terminality come from the route receipt. | Return `route_receipt identity does not match` or `producer-enforcement: semantic result contains dispatcher-owned field`. |
| R5 | Keep the existing verdict comparison unchanged. It remains a consistency check, not proof of independent production. | The gate receives a worker semantic verdict and a wrapper verdict only after the source-level producer guard passes. | Existing `verdict-mismatch`, `no-verdict` and lineage failures remain unchanged. |
| D1 | No change in this cluster. Continue using observed execution fields for delivery evidence. | Producer is the receipt execution helper; checker is the delivery validator. | Existing delivery validator failure. |
| D2 | No change in this cluster. Treat this as a separate delivery-receipt producer problem. | Route receipt and judgement evidence are both delivery artefacts, but their producer separation is not established here. | Existing delivery validation failure. |
| D3 | Retain the existing family, session and route freshness checks. | Generating attempt, grader and adjudicator have distinct provider/session/route identities. | Existing evaluation validator failure. |
| D4 | Retain host-side installed-verifier execution. Never accept the worker’s commit claim alone. | Installed verifier process and filesystem Git state produce the acceptance record. | Existing claim rejection. |
| D5 | Retain host-side GitHub, Git and CI observation. | External GitHub and local Git observations produce the merge evidence. | Existing binding or validator failure. |
| T1 | Split the fixture into named dispatcher, worker and finaliser builders. A single fixture helper must not provide both compared facts from one dictionary. | Test factories declare `dispatcher-record`, `dispatcher-terminal`, `worker-semantic` and `host-consumer` roles explicitly. | Test fails if one source object is reused for both sides. |
| T2 | Preserve the end-to-end direct-dispatch join and add a genuine collision case that the producer guard rejects. | Shell dispatcher output and semantic worker result are separate bound files. | Finaliser returns the producer-boundary failure instead of certifying the leg. |

## 4. Mechanism choice

### Chosen mechanism

Use one small shared enforcement module with two responsibilities:

1. A source-level prompt scanner that rejects self-attestation instructions.
2. A runtime semantic-result check used by `run_dir_finalize.py`.

The module should be:

`$AGENTS_HOME/.worktrees/baseline-integration/skills/orchestrate/scripts/producer_enforcement.py`

The production identity model is logical ownership, not model identity:

- `dispatcher-record`: route JSON produced by `cf_dispatch.sh` from observed dispatch state.
- `dispatcher-terminal`: terminal JSON produced by the dispatcher from observed process termination.
- `worker-semantic`: semantic terminal JSON produced by the reviewed worker.
- `review-wrapper`: the review-plan declaration and wrapper verdict.
- `host-consumer`: `run_dir_finalize.py`, which reads and derives acceptance.

The following fields are dispatcher-owned and must never be supplied by a worker prompt as self-attested output:

`adapter`, `adapter_gate`, `attempt_id`, `catalog_model`, `certification_eligible`, `cross_family`, `endpoint_provider`, `exit`, `identity_source`, `model`, `model_family`, `orchestrator_family`, `output_path`, `output_sha256`, `provider_assurance`, `provider_family`, `read_only_guarantee`, `resolved_model`, `reviewer_id`, `route_alias`, `route_receipt`, `status`, `terminal_artifact_path`, `terminal_artifact_sha256`, `terminal_observed`.

The runtime check must reject any semantic terminal object containing one of those fields. The existing closed worker schema remains the first check; the new check is a finaliser-side defence and must not replace the excluded worker-outcome validator.

The finaliser must use:

- the bound route receipt for actual adapter, provider, model, endpoint, attempt, terminal and cross-family facts;
- the bound semantic terminal for worker status, summary, reason and semantic verdict;
- the review-plan row for declared scope, lens, tier and expected binding.

The finaliser may compare review-plan declarations with the route receipt, but it must never treat the declaration as the source of actual provenance.

### Prompt scanner contract

`producer_enforcement.py` must expose a deterministic scan function and CLI:

```text
python3 skills/orchestrate/scripts/producer_enforcement.py --check <product-root>
```

The scan must:

- recursively inspect text files under `skills/`, `workflows/`, every directory named `templates`, and every directory named `prompts`;
- include `.md`, `.txt`, `.js`, `.mjs`, `.py`, `.sh`, `.yaml`, `.yml` and `.json`;
- skip `.git`, `node_modules`, cache directories and test directories;
- report `PE001` with path, line number and the matched field;
- detect an output directive such as `copy`, `return`, `report`, `restate`, `repeat`, `include`, `set`, `capture`, `provide` or `declare` applied to a dispatcher context and a dispatcher-owned field;
- treat explicit prohibitions such as `do not copy`, `never report`, `must not return` as safe;
- allow `read`, `inspect`, `verify` and `consume` instructions when they do not also instruct the worker to return or restate the dispatcher-owned value;
- scan adjacent source lines as one instruction window so JavaScript concatenated prompt strings are checked;
- return zero only when no violation exists.

The static check must be invoked by `scripts/check-harness` before the test suite. There is no allowlist for the current collision phrases. The repaired workflow text must pass the scanner as part of the direct cutover.

### Why this mechanism

This is the smallest mechanism that can actually fail against the observed defect:

- It does not trust the wording of the gate.
- It detects the input-construction path that makes the gate degenerate.
- It makes the prohibited prompt shape fail before it can reach a run.
- It leaves the existing verdict gate intact.
- It uses host-owned route artefacts for provenance and worker-owned terminal artefacts for semantics.
- It is maintainable because the field vocabulary and scan rule live in one module.

The enforcement layer exists specifically to make self-reported-provenance collisions structurally impossible to reintroduce. The existing gate already read correctly while receiving a degenerate input. The fix must therefore constrain how inputs are produced, not rewrite the comparison.

This is not identity hardening against a hostile local user. It catches the ordinary case where one agent, or one dispatcher-authored prompt, supplies both sides of a comparison.

### Rejected alternatives

- Model name, provider name or family string: these are ordinary self-reported values and do not establish producer identity.
- PID, process start time or operating-system account: unstable, platform-specific and unnecessary for a single-user harness.
- Cryptographic signatures: excessive for this local threat model and still require a trusted signer boundary.
- Hashing the same returned object twice: proves byte equality, not independent production.
- A new generic provenance registry: too much machinery for a narrow source-collision defect.
- Changing `review_terminal.py`: excluded and unnecessary because its gate is already correct.
- Requiring a second model for every comparison: does not solve a dispatcher prompt that copies the first model’s facts and adds unnecessary cost.
- A compatibility parser for old prompt or artefact shapes: prohibited by the direct-cutover requirement.

## 5. Acceptance matrix

### Test-file changes

| Test file | Current test count | New tests | Target count |
|---|---:|---:|---:|
| `tests/test_producer_enforcement.py` | 0 | 8 | 8 |
| `tests/test_run_dir_finalize.py` | 46 | 3 | 49 |
| `tests/test_harness_contract.py` | 38 | 1 | 39 |
| `tests/test_review_terminal.py` | excluded | 0 | unchanged |
| `skills/orchestrate/evals/test_cf_dispatch.py` | 53 | 0 | unchanged |
| `tests/test_worker_outcome.py` | unchanged | 0 | unchanged |
| `tests/test_worker_outcome_repair.py` | 26 | 0 | unchanged |

### Baseline and target counts

The recorded baseline is:

- Python: `1876 passed`, `2 xfailed`, `178 subtests`, `0 failed`.
- Vitest: `2357 tests` in `254 files`, `0 failed`.
- TypeScript: `2` projects, exit `0`.

The target after this cluster is:

- Python: `1888 passed`, `2 xfailed`, `178 subtests`, `0 failed`.
- Vitest: `2357 tests` in `254 files`, `0 failed`.
- TypeScript: `2` projects, exit `0`.

The local read-only environment did not contain `pytest`, so the baseline was not rerun during this design pass.

### Required new tests

| # | Test | Scenario | Assertion |
|---:|---|---|---|
| 1 | `test_rejects_copy_dispatcher_fields_prompt` | Scan `copy every normalised field from the dispatcher record and return it`. | Returns one `PE001` violation naming the source line and dispatcher field context. |
| 2 | `test_rejects_model_family_self_attestation` | Scan `report your model family, provider family and route receipt in the result`. | Returns `PE001`; all three fields are classified as dispatcher-owned. |
| 3 | `test_rejects_reviewer_id_and_record_path_self_attestation` | Scan `return reviewerId and set recordPath to the dispatcher record path`. | Returns `PE001` for `reviewer_id` and route path. |
| 4 | `test_rejects_verbatim_dispatcher_guarantees` | Scan `return the normalised dispatcher guarantees verbatim`. | Returns `PE001` for dispatcher-owned guarantees. |
| 5 | `test_allows_read_route_return_verdict_only` | Scan `read the route receipt for context, then return only the semantic verdict and anchors`. | Returns no violations. |
| 6 | `test_allows_explicit_no_self_report_instruction` | Scan `do not copy, report or restate any dispatcher field; return only findings`. | Returns no violations. |
| 7 | `test_current_repository_prompt_scan_is_red_before_repair` | Run the scanner against current HEAD, where the three excluded workflow repairs have not yet landed. | The test fails against current HEAD and identifies at least P1, P2 and P3. It passes only after the separate repair lands. |
| 8 | `test_same_agent_return_is_rejected_as_genuine_collision` | Construct one structured agent return containing a semantic verdict and copied dispatcher lineage, then feed that same object as both worker and wrapper input. | The producer-enforcement function rejects the input before certification with `producer-enforcement: self-reported dispatcher provenance`. This is the required genuine current-HEAD red test. |
| 9 | `test_finalizer_rejects_dispatcher_fields_in_semantic_result` | Bind a valid route receipt and a semantic terminal JSON containing `provider_family` and `output_sha256`. | `_validate_review_plan` returns `worker outcome rejected: producer-enforcement` and does not produce a certifying leg. |
| 10 | `test_finalizer_uses_bound_route_for_actual_lineage` | Make the review-plan declaration claim a different provider family while the bound route receipt remains valid. | Finalisation rejects the declaration mismatch and never treats the declaration as the actual producer identity. |
| 11 | `test_finalizer_accepts_clean_separated_producers` | Bind a dispatcher route, dispatcher terminal, human answer and semantic worker terminal with distinct paths and no dispatcher-owned fields in the semantic result. | Existing outcome acceptance and review finalisation pass. |
| 12 | `test_check_harness_invokes_producer_scan` | Read `scripts/check-harness` as text. | It invokes `producer_enforcement.py --check` before the full test command. |

No excluded file may be modified by this cluster.

## 6. Size estimate

Expected implementation size:

- `producer_enforcement.py`: 90 to 110 lines.
- `run_dir_finalize.py`: 20 to 30 lines.
- `scripts/check-harness`: 1 to 2 lines.
- Tests: 100 to 130 lines.
- Total: approximately 210 to 270 changed lines, including tests.

The estimate is larger than the original rough 200-line description because the acceptance tests must cover the actual current-HEAD collision, repository-wide prompt scanning and finaliser fail-closed behaviour.

Not worth building:

- No cryptographic producer-signature system.
- No process identity registry.
- No new receipt schema or migration layer.
- No rewrite of the delivery receipt producer or validator.
- No concurrency lock or load-baseline framework for issue 590.
- No command-count evidence model for issue 582.
- No Git commit-verification repair for issue 579.
- No edits to the six excluded files.
- No compatibility path for old prompts, old review records or old self-attested lineage.

## 7. Unresolved list

None. The writer lane has a fixed module, fixed scan roots, fixed field vocabulary, fixed failure codes, fixed integration point, fixed test files, fixed test scenarios and fixed scope exclusions.


