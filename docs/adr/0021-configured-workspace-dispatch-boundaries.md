# ADR 0021 — Configured-workspace provider access and dispatch boundaries

**Status:** Accepted 2026-08-29 (issue [#682](https://github.com/mblauberg/provenant/issues/682))

**Amends:** [ADR 0013](0013-thin-provenant-cli.md) and the current
orchestration, routing and workspace-access doctrine.

## Context

Provenant is a personal, single-user harness. The chair is the accountable
operator for the authorised workspace, and configured provider CLIs are useful
execution capacity. Requiring model-family separation before ordinary work adds
friction without providing the assurance that the user is actually asking for.
It also makes cheap mixed-provider batches unnecessarily difficult.

The repository nevertheless needs an honest boundary between ordinary execution
and an assurance claim. It also needs one owner for dispatch mechanics, while
keeping Fabric small and keeping delivery acceptance separate from exploratory or
batch execution.

## Decision

### Owner map

| Concern | Current owner | Boundary |
|---|---|---|
| Model and tier resolution | `scripts/model-route` | Resolves configured routes and records route identity; it does not launch providers. |
| Provider-invocation adapter | `skills/orchestrate/scripts/cf_dispatch.sh` | Invoked by ordinary and assurance routes; its distinct-family requirement remains assurance policy. |
| Ordinary dispatch runner and interface | `skills/orchestrate/scripts/dispatch_run.py` (implemented by [#690](https://github.com/mblauberg/provenant/pull/690)) | Owns one ordinary intent/policy attempt and its `attempt.json`, and delegates provider invocation to `cf_dispatch.sh`. |
| Fixed bounded batches | `skills/orchestrate/scripts/batch_run.py` (implemented by [#692](https://github.com/mblauberg/provenant/pull/692)) | Builds on `dispatch_run.py` for fixed task sets, bounded concurrency, partial results and explicit retry; it does not own provider invocation or workspace policy. |
| Coordination | `runtime/fabric` | Mailbox, shared tasks and activity only; no provider launch, scheduler or lifecycle owner. |
| Dispatch evidence | `attempt.json`, indexed by existing `MANIFEST.md` and validated during `run_dir_finalize.py` finalisation | Records execution attempts, route lineage and observed completion; it is not delivery acceptance and must not create a parallel lifecycle ledger. |
| Delivery evidence | `deliver` and canonical delivery `RUN.json` | Records artifact verification, review and acceptance; it may reference the orchestration receipt. |

The dispatch runner provides the ordinary intent/policy interface and the
ordinary single-dispatch intent/policy mode; the batch layer provides the fixed
bounded implementation boundary. The existing
distinct-family behaviour in `cf_dispatch.sh` remains the assurance path, while
ordinary execution explicitly records that it makes no independence claim.
Credential/auth-store exclusions, unrelated-path containment, explicit denials,
write/resource limits and external-action gates remain implementation acceptance
gates for both runners.

### Configured-workspace access

An authorised chair may dispatch ordinary workspace work to any configured
provider family. Model-family separation is not an execution permission gate.
The selected adapter, provider and model remain visible in the dispatch
manifest so an assurance claim can distinguish same-family work from genuinely
independent work.

This broad default still excludes credential and authentication stores, secrets,
unrelated paths and explicitly denied content. It remains bounded by the
approved workspace path, write scope, resource limits, platform rules and
external-action gates. A narrower task or project policy may further restrict
the default.

### Execution and assurance

Execution asks whether a configured provider may perform the requested work.
Assurance asks what can be claimed about the resulting work. Ordinary work may
use one family, several workers from one family, or a mixed provider batch. A
claim of independent or cross-family verification must record and satisfy its
own route and evidence requirements; it does not restrict ordinary execution.

Receipts record actual provider/model lineage and capability status. They never
turn a route record, a worker report or agreement between outputs into proof of
correctness by itself.

### One dispatch owner

The orchestration adapter layer is the sole behavioural owner for provider
dispatch, process waiting, cancellation, retry and batch mechanics. `provenant`
is the stable operator front door and may expose bounded `dispatch`, `batch` and
`run` inspection commands, but it delegates to that owner. It must not create a
second adapter parser, scheduler, lifecycle database or delivery receipt.

Fixed bounded batches are a first-class capability: fixed task sets,
concurrency limits, per-task timeout, partial results and explicit retry
attempts. Adaptive waves and reducers may be layered on that interface;
they do not create a new runtime authority.

### Compact dispatch manifests

The compact dispatch manifest is the canonical attempt record for ordinary
dispatch and batch execution. Existing orchestration `MANIFEST.md` indexes each
`attempt.json`; `run_dir_finalize.py` validates that evidence while finalising
`RUN_RECEIPT.json` custody and terminalisation. It does not copy attempt
references into the receipt or create a new owner or parallel lifecycle ledger.
Each task attempt records enough to
reconstruct what happened: task and attempt IDs, requested and resolved route,
actual provider and model, workspace and base identity when available,
start/end, status, exit information, prompt/result paths and digests, and retry
lineage.

This compact dispatch manifest is not a delivery `RUN.json`. It answers “what
executed?” and may contain partial or failed exploratory work. The canonical
delivery receipt answers “was the resulting artifact verified and accepted?”
When a dispatch produces a governed deliverable, the delivery `RUN.json` may
reference the orchestration receipt; neither record replaces the other.

Exact prompts and results are retained once in the local run directory according
to the active retention policy. Receipts and Fabric messages carry paths,
digests and compact status rather than duplicate transcripts.

### Fabric remains coordination-only

Fabric remains the project-scoped mailbox, shared task ledger and activity log.
It may carry dispatch requests, correlations and status, but it does not launch
providers, own provider sessions, schedule batches, wait on processes or decide
delivery acceptance. Direct official provider CLIs remain the execution
boundary.

## Consequences

- Luna, Gemini Flash and other configured workers can be used freely for cheap
  swarms and mixed-provider batches.
- Cross-family evidence remains available when a user or assurance profile needs
  it, without imposing that cost on every run.
- A single dispatch owner prevents drift between `provenant`, Fabric and
  provider-specific scripts.
- Partial batch completion is useful and inspectable without pretending that a
  delivery receipt exists.
- Workspace trust stays simple for the common case, while sensitive stores,
  unrelated paths, explicit denials and external actions retain their existing
  gates.

## Non-goals

This decision does not implement a provider adapter, scheduler,
daemon, recursive manager tree, automatic fallback, silent substitution or
unbounded raw-output retention. Those are implementation decisions under this
boundary and must preserve its ownership and evidence rules.

## Acceptance criteria for implementation

1. Ordinary configured provider dispatch accepts same-family and mixed-family
   workers without requiring a family-separation flag.
2. Credential/auth-store exclusions, unrelated-path containment, explicit
   denials, write/resource limits and external-action gates remain testable.
3. One dispatch owner produces append-only compact manifests with observed
   process completion and retry lineage.
4. Batch status preserves partial results and individual failure states.
5. Delivery `RUN.json` remains distinct and may reference the orchestration
   receipt that indexes attempt records.
6. Fabric tests continue to demonstrate coordination-only behaviour.
7. `provenant` exposes only delegated bounded commands; provider mechanics are
   not duplicated in the front door.
