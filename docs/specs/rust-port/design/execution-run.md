# Execution: runs

Status: proposed for chair acceptance and the command-tree freeze in
`design/cli.md`. Author route: `codex/gpt-6.1-sol@high`, via Fabric.
Governing rules: [PORTING.md v7](../PORTING.md), especially R0, R1 and R9;
dependencies: [ADR 0028](../../../adr/0028-rust-port-dependencies.md), decision 3.
Audience: the `provenant-run` build lane and the Fabric, routing and supervisor
owners. Names below are proposed Rust interfaces, not existing APIs.

## Purpose and ownership

`provenant-run` turns a request into bounded provider attempts, retains the
exact prompt, route and result of each attempt, and makes completion and
cancellation reliable across a disconnected caller. It owns admission, batch
scheduling, retries, attempt lineage and run finalisation. `fabric-runtime`
owns agent identity, placement and the searchable run registry; its execution
facade calls run for dispatch, batch, status, wait, cancel and output.
`supervisor` owns all process custody, capture, observation and signalling.
Routing resolves models and effort; providers build launch specifications and
interpret their own wire formats. Run neither starts processes directly nor
implements a second route catalogue.

## Product decisions and evidence

The source baseline is commit `1b3d48e4c549811ce3636edc30b047f36fd24ada`.
The inventory in this document is against that commit. Source evidence includes
`skills/orchestrate/scripts/`, `scripts/fabric_lanes.py`, the front-door dispatch
and `lane` routes, their tests and Fabric's execution facade. Earlier Rust
`run::{detached,lease,lineage}` contains useful invariants, but its Python
coercions, temporary OS helpers and old record layouts are discarded.

### Observed use

A tool-input audit on 24 October 2026 covered 2,444 JSONL transcripts under
`~/.claude/projects` and `~/.codex/sessions`. It counted command-position shell
invocations, paired Claude calls with results and inspected completed Codex
command wrappers; heredoc bodies, file edits and conversational mentions were
excluded. Counts establish attempted invocation, not successful execution;
conditional commands inside a Codex wrapper may not have run. The corpus spans
several projects and is a usage sample, not a production counter.

| Recorded surface | Invocations or supplied fields |
| --- | --- |
| `provenant lanes` | 561; `--wait` 447, `--timeout` 447, `--all` 161, `--summary` 12 |
| `provenant fabric dispatch` | 31; `--operation-id` 26, model/effort/mode 26 each, worktree 24 |
| `fabric_dispatch` | 1106; tasks arrays 41 (sizes 1–4), resume 9, handoff 0; model 959, effort 285, mode 1043, worktree 378 |
| `fabric_status`, `fabric_output`, `fabric_cancel` | 269, 31, 16 respectively |
| Old `provenant run` verbs, `provenant batch`, direct `run_controls.py` | None found |
| Direct `dispatch_run.py`, direct `cf_dispatch.py` | 7 and 1 command candidates; not evidence of a required public bypass |

Reproduction locators, with prompts omitted: Claude project
`-Users-mikey-Repos-provenant/1c21e420-fb1c-4a3f-be95-eb42e8489edb.jsonl`
contains lane waits and Fabric dispatch/cancel on 23 October. Codex
`sessions/2026/10/23/rollout-2026-10-23T12-12-51-01a16688-4ecc-71f2-92a4-3178987a3ffe.jsonl`
contains the direct-dispatch command candidate. These are research locators,
never runtime dependencies. No credentials or authentication stores were read.

### Keep

| Capability | Product requirement and source evidence |
| --- | --- |
| Read-only and worktree-write dispatch | `orchestrate/SKILL.md`, `routing-and-tiers.md` and `dispatch_run.py` use both. Confinement proves the worktree and permissions; one writer holds an OS-backed lease until custody ends. No primary-checkout writer. |
| Explicit model and effort, resolved provenance | Daily Fabric requests select routes. Store requested, resolved, applied and provider-observed values separately. Never fill an unknown model or effort from a guessed default. |
| Dispatch, bounded batch, operation IDs | Fabric `execution.ts`, `batch-execution.ts` and `fabric_lanes.py` need retriable launches without duplicate work. Keep 1–64 independent tasks, default concurrency 4, maximum 8. |
| Status, waiting on lanes, cancel and bounded output | `chair-loop.md`, `worker-liveness.md` and the CLI lane waiter are the chair's control loop. Waiting or losing a reader never stops work. |
| Exact attempts and continuation | `run_controls.py` and Fabric `resume.ts` depend on retained prompt/result identity. Every retry or continued provider turn creates a new attempt; completed attempts never change. |
| Nested runs and scoped cancellation | `run_lineage.py`, nested-dispatch tests and worker guidance require parent/child attribution and stopping owned descendants without touching siblings or the chair. |
| Host admission and context observations | `dispatch_run._launch_dispatch` calls memory admission on every launch. Provider streams supply context/usage; admission protects the host even when callers never name it. |
| Detached run owner | Fabric launches owners that survive MCP shutdown. Keep that lifetime through the same binary and supervisor API, rather than a generic command wrapper. |

### Remove or simplify

| Removal | Evidence and replacement |
| --- | --- |
| Direct `cf_dispatch.py`, `--chain`, `--plan-only`, `--doctor` | The rewrite README explicitly retires the direct entry. `dispatch_preflight.py` shells out to it and also has an in-process fast planner. Replace both paths with routing and providers library calls; move utility callers to run or delete them with their owner. |
| Duplicate legacy and Fabric attempt trees, digest sidecars and Markdown manifest bookkeeping | `dispatch_run.py` publishes both `dispatch/tasks` and `tasks`; controls validate bundles across rename gaps. One typed attempt record publishes references and digests atomically. These files are product-owned, not external contracts. |
| Generic run scaffolding, pane inventory, paired-primary driver lease and manual gate finaliser | These helpers mix scratchpads, tmux ownership and delivery gates into execution. Workflow owns their surviving callers and decides the paired-primary policy; this is a boundary removal, not a claim that workflow is unused. Replace or retire those contracts there before deleting shared helpers. Run closes from typed attempts and child custody, retaining evidence without automatic pruning. |
| `inspect`, `show`, `prompt`, `result`, `diagnostic-log` as separate verbs | Their implementation overlaps status and artifact reads. Consolidate into `status` and `output --part`; actual use of a spelling does not require an alias in the new tree. |
| `reduce` and synthetic reduction prompts | No transcript invocation or skill caller was found beyond help/tests. `run_controls._reduce` constructs a special prompt. Dispatch a normal read-only task with explicit retained result references. A batch reports every failed/omitted task; it never implies a successful council. |
| Arbitrary detached commands, PID text markers and `--validate` | `run_worker_detached.py` and Rust `detached.rs` implement a second completion protocol. The facade needs durable provider runs, not an arbitrary argv launcher. Use supervisor-owned workers and typed completion. |
| JSON fallback lists, `fallback:any`, hidden effort downgrade and automatic writer replay | `exec_routing.py` merges several candidate representations; `dispatch_preflight.effort_retry_plan` silently retries rejected effort. Routing supplies a bounded candidate plan. Only opted-in read-only transient failures retry automatically. Every changed route/effort is visible in a new attempt. |
| Runtime reads of Codex session transcripts | `context_usage._codex_rollouts`, `codex_rollout_effort` and `with_codex_rollout` supplement streams from `~/.codex/sessions`. R1 forbids transcript reads by the rewritten runtime. Unknown context or observed effort stays unknown. Recorded, sanitised wire fixtures remain tests. |
| Alias coercion, typo repair, old-root discovery and record migration | `execution-input.ts` repairs route names; controls select newest matching task across old directories. Use strict typed requests and unambiguous IDs. New state starts empty; never scan, migrate or remove `.agent-run/`, `~/.agents` or transcripts. |
| Watchdog silence as an outcome or stop request | Silence is an observation. R5 and lane conformance require a healthy silent provider to survive; only supervisor's closed stop reasons can signal it. |

## Proposed commands and MCP

All public execution commands are under `provenant run`. Top-level `dispatch`,
`batch` and `lanes`, and `lane run`, retire rather than become aliases. Update
skills and surviving callers in the build PR replacing each command. Fabric's
public tool names and JSON-RPC rendering belong to `design/fabric.md`; run adds
no MCP server or tool registrations.

Every command accepts `--json` and the state-root override defined by the state
and CLI designs. Requests use integer seconds and bytes, never floating point.

| Verb | Arguments and flags | Successful output |
| --- | --- | --- |
| `dispatch` | Exactly one `--prompt-file PATH` or `--prompt-stdin`; optional `--task-id ID`, `--operation-id ID`, `--host HOST`, `--delivery-run-id ID`; route and access flags below; `--timeout N`, `--wait N` | Acceptance receipt with run/task/attempt IDs, operation ID, exact `Route:` line, state and artifact directory. Default wait 0; wait bounded to 55 seconds. |
| `batch` | `--manifest PATH`, `--concurrency N`, `--operation-id ID`, `--host HOST`, `--wait N` | Run ID, per-task acceptance/refusal, counts and attempt/result references. A bad manifest rejects before any launch. A task-local route/admission refusal does not suppress valid siblings. |
| `status` | `RUN[/TASK[/ATTEMPT]]`; optional `--tail N` (0–500, default 0) | State, reason, exact attempt route, custody observation, timestamps, result reference and optional bounded result tail. No repair or provider signal. |
| `lanes` | Optional exact selectors, `--project PATH`, `--all`, `--limit N`, `--cursor TOKEN` | Active lanes plus five most recent terminal lanes by default, with explicit pagination. `--all` includes retained terminal lanes. Registry selection belongs to Fabric. |
| `wait` | Exact selectors or current agent's lanes; `--project PATH`, `--all`, `--timeout N`, `--summary` | By default report the next unseen terminal/attention change. `--all` waits for all selected lanes. Summary is one line per attempt. Default timeout 55 seconds; explicit CLI timeout 1–86400 seconds. |
| `cancel` | Exactly `RUN`, `RUN/TASK` or `RUN/TASK/ATTEMPT`; `--operation-id ID`, `--reason TEXT` (max 256 bytes), `--wait N` (0–55, default 5) | `requested`, `already_terminal` or `unobservable`, selected scope and remaining custody. Accepted request is not proof that a process exited. |
| `output` | `RUN/TASK/ATTEMPT`; `--part result|prompt|stderr|events` (default result); either `--offset N` or `--tail N` (default 40, max 500); `--max-bytes N` (1–20000, default 20000) | Bounded bytes/text, part, offset, next offset, EOF, clipping and completeness. Prompt access requires the same project authority as dispatch. |
| `retry` | Exact terminal `RUN/TASK/ATTEMPT`; `--operation-id ID`; `--same-route` or a new route selector; `--continue` or `--fresh`; optional prompt input, `--timeout N`, `--wait N` | New run/attempt IDs and exact `retry_of`; the old run stays closed and its receipt/result immutable. `--continue` requires new prompt input and an exact provider continuation handle. `--fresh` with a new prompt starts a fresh session and records the prior result reference. |

Route flags: exactly one `--model ADAPTER/ID` or `--route POOL`, optional
`--effort VALUE`, `--account LABEL`, `--confidential`, `--intent ordinary|assurance`
and typed assurance inputs (`--orchestrator-family`, `--reviewer-id`,
`--risk-tier`). Routing owns their validation, requested/applied distinctions
and assurance eligibility. Explicit unsupported selectors fail with a fix.
No unrestricted `--knob`, raw provider argv, `--allow-secrets` or sandbox bypass.

Access flags: `--mode read-only|worktree-write` (default read-only), `--cwd PATH`,
`--worktree PATH` required for a writer, repeatable `--read-root PATH` and
`--write-root PATH`, `--network on|off`, repeatable `--capability NAME`.
Confinement validates these requests against caller authority; flags never
grant authority themselves. Artifact writes use narrowly granted attempt
paths. A read-only provider must not receive write access to its custody files.
`--context-ceiling N` is a typed provider hint, default 300000 tokens, range
100000–1000000. It can lower an auto-compaction threshold, never raise one.
`--fallback none|pool` defaults to none and is allowed only in read-only mode.

A manifest is strict JSON `{schema_version:1,tasks:[...]}`. Each task has a
unique ID (1–64 ASCII letters/digits/`._-`, starting alphanumeric), one inline
prompt or prompt-file reference, route, access and optional delivery-run ID.
No implicit merging of contradictory route fields or per-task hosts: one
batch has one placement. Duplicate or overlapping canonical writer scopes
reject the whole manifest. A delivery ID is an opaque workflow correlation,
not delivery acceptance. Maximum manifest size is 1 MiB, including inline
prompts; dispatch prompt size is also 1 MiB. Resolve and snapshot files once
through confinement before submission.

The facade resolves project/agent/host and selectors through `fabric-runtime`,
then calls run with exact IDs. Ambiguous task names fail with candidate IDs;
there is no 'newest matching task' rule. `wait --all` snapshots active/queued
lanes at entry plus already-unseen completions, excluding its caller and
ancestors. Later launches belong to the next wait. An empty selection succeeds
immediately. Input-required counts as attention-ready, not success. Unreachable
hosts remain unknown and cannot satisfy an all-lanes wait. Wait cancellation
ends only the reader. Fabric stores per-agent seen revisions and advances them
only after output was written; a clipped report leaves undisplayed rows unseen.

Human output is concise, with full output paths and exact route provenance.
JSON uses versioned envelopes with `schema_version`, `operation_id`, `run_id`,
`tasks`, `state`, `outcome`, `warnings` and `error:{code,message,fix}` as
applicable. Output slices additionally report byte offsets; log text may use
UTF-8 replacement with `encoding:"utf8-lossy"`, while offsets always count
original bytes. Result and prompt bytes must be valid UTF-8. Diagnostics go to
redacted stderr, never JSON/MCP stdout. Unknown fields, negative numbers and
out-of-range values are errors, not silently clamped inputs.

Exit codes: 0 for accepted operations, successful reads, completed successful
waits and already-terminal cancellation; 1 for an observed unsuccessful
terminal run/task; 2 for invalid input, unsupported version, missing or
ambiguous target; 3 for input-required on a completed wait; 4 for refusal,
contention, unobservable custody or unavailable state; 124 for a reader wait
deadline; 130 for an interrupted reader. Provider exit codes remain record
fields. A provider runtime timeout is outcome `timed_out` with exit 1, distinct
from a wait deadline. Mixed waits prefer 124 if incomplete, otherwise 1 over 3.
An accepted detached launch still exits 0 if it has not finished.

## Records, layout and publication

`StateRoot` comes from `design/state.md`; this area does not pick a second
home or fall back to an old root. All paths below are relative to that fresh
root. Run IDs are opaque core IDs, not timestamp suffixes or PID aliases.

```text
runs/<run-id>/
  request.json                 # immutable, canonical request and prompt digests
  run.json                     # run/task summary, revision, lineage and closure
  owner.json                   # supervisor-issued worker custody reference
  control.json                 # durable scoped cancellation requests
  tasks/<task-id>/attempts/<u64>/
    attempt.json               # sole authoritative attempt record
    prompt.txt                 # immutable exact provider prompt
    result.md                  # sealed result, even for unsuccessful attempts
    events.jsonl               # bounded, redacted provider wire capture
    stderr.log                 # bounded, redacted diagnostic capture
    supervisor.json            # supervisor's durable observation/exit evidence
    partial/                   # unsealed provider artifacts, never success evidence
  locks/                       # short publication/sequence locks via supervisor
operations/<operation-id>.json # run-owned local idempotency receipt
admission/                     # host-wide queue tickets and lock, across projects
```

The state design owns private directory/file modes (0700/0600) and root
resolution. Fabric owns its registry database and agent records elsewhere
under the same root. Registry rows contain run references/revisions, not a
second mutable attempt record. Shared records live in `provenant-records`:

- `RunRecord`: schema version, run/project IDs, caller agent reference,
  operation ID, kind, immutable request digest, task IDs, parent attempt,
  child run references, revision, created/closed times and closure state.
- `AttemptRecord`: run/task/attempt IDs, optional batch and delivery IDs,
  retry/continuation parent, requested/resolved/observed route and effort,
  confinement digest, workspace identity, deadlines, admission reason,
  supervisor custody reference, provider session reference, timestamps,
  state/outcome/reason, usage/context with source, artifact paths/digests,
  capture clipping, redaction state and typed publication errors.
- `OperationRecord`: project, caller scope, operation kind, request digest,
  allocated IDs and `prepared|accepted|terminal|launch_unknown` state. The
  request digest excludes reader wait/rendering options, but includes prompt
  bytes, route request, authority scope and execution controls.
- `ControlRequest`: operation ID, exact target IDs, scoped cancellation
  reason, issuing principal, revision and receipt time. `AdmissionTicket`
  holds owner custody, monotonic sequence and integer resource thresholds.

All records use `schema_version:1`, strict fields and tagged enums. Operation
IDs are 1–128 ASCII letters/digits/`._-`, starting alphanumeric. Unknown versions fail closed without modification. Future incompatible records require
an explicit versioned reader; R1 supplies no old-state reader or import here.
Integers are `u64`/`i64`; percentages use integer basis points. Canonical
request/digest encoding belongs to records. Paths in records are relative
contained paths; native paths remain `OsString` until validated for recording.
JSON records are capped at 1 MiB. Captures are capped at 64 MiB per stream;
continued parsing after capture clipping still observes provider completion.
Results are capped at 4 MiB: an oversized result is a typed failure and cannot
be silently certified from a truncated file. Every terminal attempt has a
`result.md`, possibly empty, with a completeness flag and digest. Success also requires supervisor-observed exit 0 and a matching provider
identity/outcome; only a complete, non-empty, validated result can support it.

Run closure is `open|closing|closed`; closing can wait on registered children.
Attempt states are `queued -> starting -> running -> settling -> terminal`;
queued cancellation/refusal and spawn failure can terminate before running.
`input_required` is a terminal attempt outcome, allowing a new continued turn.
Other outcomes are `succeeded`, `failed`, `rejected`, `cancelled`, `timed_out`,
`rate_limited`, `quota_limited`, `auth_required`, `model_unavailable`,
`permission_denied`, `quarantined` and `lost`. A dead owner is not a failed
provider: unresolved custody is an observation until supervisor proves exit
or loss. Valid natural completion wins over a late cancellation or deadline.

Publication order is load-bearing:

1. Under an operation lock, atomically reserve IDs and request digest. Reusing
   an ID with the same request returns the recorded acceptance; changing its
   project, scope or request is `operation_conflict`. Persist this before spawn.
2. Publish the request and queued attempt, acquire admission and writer leases,
   then hand the supervisor an immutable launch plan and publication reservation.
   A lost launch acknowledgement becomes `launch_unknown`, never a blind retry.
3. Keep live progress as replaceable observations. Seal result and supervisor exit
   evidence first, then atomically replace `attempt.json` with its complete
   terminal record. A referenced artifact is already fsync'd and hashed.
4. Update run summary and operation receipt, then emit a revisioned event for
   Fabric's registry and telemetry. Both consumers deduplicate by attempt ID
   and revision. Lost registry updates are repairable from run-owned files.
5. Close the run when all tasks and registered children are terminal. Transfer
   of child ownership, if Fabric permits it, must be recorded explicitly.
   Finalisation never deletes evidence, cleans worktrees or decides delivery.

Every record replacement, result sealing, operation reservation, control write
and queue-ticket write is atomic and fsync'd, including its containing
directory, through supervisor's safe filesystem API. There is one publication
writer per run and short locks for concurrent control submissions. A terminal
attempt is immutable; recovery may complete missing summaries/index events but
cannot replace its outcome. Append-only capture files need no atomic rename per
chunk, but cannot be authoritative completion evidence.

Reserve terminal-publication capacity before launching. ENOSPC, capture faults,
a dropped observer and index/telemetry write failures never create a stop
reason. Supervisor retains custody and raw terminal facts until durable
publication is acknowledged; after a transient write failure, retry from those
facts. Persistent failure remains `settling` with `publication_pending:true`; custody
and writer reservations remain held for safe recovery. Never report success or
release a writer early to make a bookkeeping failure disappear. No design can
promise successful disk publication during permanent storage failure.

## Library interfaces and dependencies

The following signatures describe responsibilities, not final Rust spelling:

```rust
dispatch(ctx: CallerContext, req: DispatchRequest) -> Result<Acceptance, RunError>
batch(ctx: CallerContext, req: BatchRequest) -> Result<BatchAcceptance, RunError>
status(ctx: ReadContext, target: ExactTarget) -> Result<RunSnapshot, RunError>
wait(ctx: ReadContext, targets: Vec<ExactTarget>, until: WaitUntil, deadline: Deadline,
     reader_cancel: ReaderCancellation) -> Result<WaitResult, RunError>
cancel(ctx: CallerContext, req: CancelRequest) -> Result<CancelReceipt, RunError>
output(ctx: ReadContext, target: AttemptRef, slice: OutputRequest) -> Result<OutputSlice, RunError>
retry(ctx: CallerContext, req: RetryRequest) -> Result<Acceptance, RunError>
reconcile(ctx: CallerContext, target: ExactTarget) -> Result<Vec<RunEvent>, RunError>
```

Read contexts carry project authority; run checks it before opening artifacts.
`RunError` distinguishes validation, refusal, contention, cancellation,
unsupported continuation, launch uncertainty, version, publication and I/O
errors with causes/paths. Expected provider failures are outcomes. No library
imports `cli` or renders a command's exit code. All blocking calls have an
explicit monotonic deadline; no SQLite transaction spans a wait. `reconcile`
is an explicit mutation called by recovery, separate from read-only status.

Required owner handshakes, to be answered in the parallel designs:

| Owner | Required contract |
| --- | --- |
| Fabric (`design/fabric.md`) | Supplies authenticated `CallerContext {project,agent,host,authority,parent_attempt}` and resolved exact targets; owns registry IDs/search, placement, host qualification and seen cursors. Calls dispatch/batch/status/wait/cancel/output at the destination. Its resume or named-session continuation branch calls `retry(ContinueExact)` with the exact prior attempt and provider session; the returned new run updates the session alias. A fresh turn uses `retry(Fresh)` with an explicit new prompt. Receives `Accepted`, `AttemptChanged`, `RunClosed` events and enumerates run revisions for repair. Local operation receipts stay with run; Fabric persists remote operation placement/uncertainty and never reassigns an uncertain writer to another host. Cancellation of MCP calls cancels only waits/readers. |
| Routing (`design/execution-route.md`) | `resolve(RouteRequest, Authority, Deadline) -> ResolvedRoute`; optional bounded `RetryPlan` of at most two alternatives, with account scope, readiness time, applied effort and disclosure/assurance policy. Run executes the plan, without reconstructing candidates or silently altering effort. |
| Providers (same design) | `prepare(ResolvedRoute, ConfinedPlan, Prompt, Continuation) -> AdapterLaunchSpec`; incremental `observe(bytes) -> ProviderEvent`; `finish(ExitObservation, artifacts) -> ProviderOutcome`. Launch spec has argv, in-memory environment, input, continuation capability and optional context-ceiling control. Typed events report exact model/session/usage and terminal answer/question. Providers never call run or inspect run records. |
| Supervisor (`design/supervisor.md`) | `launch(OwnedLaunch, CapturePlan, PublicationReservation) -> CustodyHandle`; `observe(handle, deadline) -> Observation`; `stop(handle, StopReason) -> StopReceipt`; `recover(CustodyRef) -> Reattached|Exited|Unknown`; detached same-binary worker startup with durable custody handoff. Observe-before-spawn, unforgeable identity, WNOWAIT until cleanup, reaping last, bounded capture and terminal reserve are mandatory. Return explicit alive/exited/unknown states and distinguish stop request from observed exit. Only supervisor owns process handles, locks, signals and safe atomic files. |
| Confinement | Validated prompt snapshots, `ConfinedPlan`, canonical worktree/write scopes and lifetime-bound writer lease. Refuses secret/auth-store inputs, primary-checkout writers and unsupported enforcement. Grants only validated read/write/capability roots; performs quarantine and Git evidence checks. |
| Config, quota, telemetry | Config supplies immutable policy snapshots; quota supplies account-scoped readiness/cooldown observations; telemetry consumes idempotent usage events. Admission resource probes and owner liveness come through supervisor's safe OS API. These failures cannot signal a provider. |

There are three process roles: the transient CLI/MCP caller, a detached run
scheduler, and a dedicated attempt holder for each launched provider. Both
workers use private entries of the sole `provenant` binary, accepting validated
run/attempt references and a custody handoff, never arbitrary argv. The holder
composes run's provider observer and terminal finaliser with supervisor's safe
OS loop; supervisor does not import run or providers. It accepts the writer
reservation and publication obligation before acknowledging launch. The caller
then may disconnect. Submission acceptance means the scheduler owns the durable
request; a running attempt additionally requires the holder acknowledgement.
Scheduler death leaves holders able to finish and publish.
Recovery reconstructs the batch summary from their committed attempts.

Holder death is different: `supervisor::recover` must verify custody before
reattaching or proving exit. Otherwise status reports custody unknown, blocks
relaunch/finalisation and preserves evidence. Only proven exit/loss can produce
`lost`, never a guessed success. A durable worktree reservation survives holder
death: an available OS lock alone never authorises a new writer. No holder-death
guarantee assumes a daemon or a surviving pipe reader. This protocol must be
accepted by supervisor and confinement before the first launch PR.

Secret environment values travel only in memory/pipes. Confinement scans prompt
and result material; supervisor redacts known secret values across stream chunk
boundaries before capture or diagnostics reach disk. Provider parsers may see
original bytes in memory, but only redacted records reach telemetry or output.
If safe capture fails, omit that capture with a typed warning and keep observing
the provider; never persist an unredacted fallback or turn it into a stop request.

Admission uses a host-wide lock and resource samples, not scanning attempt
receipts across projects. Queue tickets have supervisor-verified liveness;
dead owners relinquish a place. Defaults are five per cent available memory
for readers, ten for writers and 500 MiB disk headroom for either. Retain a
20-second startup spacing window to avoid simultaneous memory spikes. A
failed probe or unavailable lock queues until the request's admission deadline,
then refuses, rather than launching unchecked. Queueing, routing, startup,
execution and retries share the explicit timeout budget (default 3600 seconds
read-only, 10800 writer); record queue time separately. No live provider is
stopped because a later memory or disk sample falls below a threshold.

Manual retry and continuation allocate a new RunId, with `retry_of` pointing
to the exact old AttemptRef. Closed runs never reopen. Automatic read-only
fallback alone appends an attempt inside the original run/task. All retries
require observed prior exit and completed cleanup. `--continue`
preserves route, account, confinement and exact provider session; unsupported
or missing sessions fail without a fresh launch. A new route or fresh session
is explicit. Replaying a successful attempt without new input is refused.
Input-required continuation retains the question and answer in
its new prompt. Opted-in read-only fallback follows only typed transient
rate/quota/model-unavailable outcomes, within the original budget and at most
three attempts. Authentication, permission denial, quarantine, partial output
and unknown custody never trigger automatic retry. Writers require an explicit
retry even after a transient failure, with fresh worktree evidence.

Lineage uses explicit `AttemptRef` and a supervisor-verified inherited parent
capability. Bad inherited ancestry is an error, not silently ignored metadata.
Register a child under the same lock that closes its parent; reject a child
registration after closure. A parent's cancel fans out cooperative requests to
its registered children via the facade, each child retaining its own custody.
Never discover authority from ancestry guesses, environment PIDs or a
provider-written journal. Foreign/remote child refs carry host identity and
unknown reachability; process-tree scanning cannot authorise stopping them.

## External contracts and fixtures

Run owns no provider wire dialect or harness hook protocol. Providers parse
real argv/environment/JSON streams and publish typed events; hooks call the
facade using their own harness-defined stdin/stdout/exit contracts. A run
receipt never claims hook or assurance certification from a prompt assertion.

Retain sanitised real stream examples from `tests/fixtures/fabric-context/`
for supported adapters (Claude, Codex, Cursor, OpenCode, agy, Kiro and Copilot
only if execution-route keeps them). In particular `claude-direct-haiku.jsonl`
contains repeated assistant frames, `codex.jsonl` turn totals and
`codex-rollout.jsonl` a recorded session example. The latter may document a
wire shape but must never cause a runtime transcript reader to return.
Provider owners record CLI/version, argv, redacted environment keys, exit and
stream provenance in new provider fixture manifests. Missing external samples
must be recorded before enabling the affected adapter, without unauthorised
live calls in this design lane.

Integration fixtures test run against typed provider events plus a fake CLI;
they do not replay product-owned `fabric-v1` receipt bytes as an oracle.
MCP fixtures are sanitised real client initialise/tool-call/cancel exchanges,
owned by fabric-mcp, and verify that tool cancellation leaves a detached run
alive. Agent Skills `SKILL.md` YAML frontmatter remains the external format;
records' subset reader tests every tracked skill. Update orchestration command
examples and test their new behaviour, not exact old prose. Git subprocess
forms and worktree identity are tested using real temporary repositories and
recorded NUL-delimited Git outputs in confinement. No provider credentials,
user transcript contents or old state files enter the Rust test state root.

## Tests and acceptance (R9)

- Unit tests beside modules use explicit clocks, requests and resource samples:
  legal/illegal transitions; exact integer bounds; strict versions; canonical
  operation digest/conflict; sequence allocation; batch partial refusal;
  cancellation scopes; retry/continuation eligibility; context missing versus
  observed/estimated; admission expiry and stale-ticket removal.
- Binary integration tests drive the real proposed command tree and MCP facade
  against an isolated state root and fake provider executable. Cover both
  access modes, manifest rejection before launch, bounded concurrency,
  operation replay, per-attempt result paths, fake-secret redaction across chunks,
  pagination/offsets, UTF-8 cuts,
  wait any/all and seen cursors, refusal/exit codes, continuation and changed
  route receipts. Place hostile sentinel old roots beside the new root and
  prove no reads, writes or migration occur.
- Process tests run on Linux and macOS with FIFO/handshake synchronisation:
  immediate child exit after observation registration; silent healthy provider;
  reader/MCP death; scheduler death with surviving holder; holder death with
  reattach/unknown and writer-relaunch refusal; observer death; publication faults
  including ENOSPC then recovery; one writer across independent callers;
  lease held through cleanup; queued and running cancellation; natural exit
  racing cancel; grandchild cleanup without stopping a sibling/shared chair;
  concurrent child registration and parent closure; remote uncertainty with no
  duplicate writer launch. Assert durable terminal facts/results before reaping.
- Re-express custody invariants from `tests/lane_conformance/` before deleting
  that suite. Delete judges against old Python/TS outputs, coercion tests,
  `pending` tests and validators replaced by Rust types. Real external wire
  fixtures remain contracts; the old implementation is never the oracle.

The build lane runs formatting, targeted clippy and touched crate tests
locally. CI runs the Rust workspace on Linux and a release build/process tests
on macOS plus surviving Python/TS tests. Gate acceptance on those PR checks;
no full local harness run or live provider dispatch is required here.

## Deletion inventory and expected size

Counts include blank/comment lines, measured with `wc -l` at the baseline
above. This is an implementation deletion plan; this design PR changes only
this document. A mixed helper is deleted only after each surviving concern has
its Rust owner. Do not retain a Python/TS adapter while waiting for another
owner; delete old-subject tests and update surviving callers as R10 requires.

Whole run sources to remove (`skills/orchestrate/scripts/` prefix):

| File | Lines | Step |
| --- | ---: | --- |
| `dispatch_run.py` | 2483 | 2 |
| `dispatch_preflight.py` | 584 | 2 |
| `memory_admission.py` | 340 | 2 |
| `batch_run.py` | 933 | 3 |
| `run_controls.py` | 1192 | 4 |
| `run_lineage.py` | 656 | 4 |
| `run_records.py` | 455 | 1 |
| `layout.py` | 245 | 1, jointly with state |
| `attempt_evidence.py` | 107 | 1 |
| `context_usage.py` | 364 | 4, parsing already supplied by providers |
| `run_worker_detached.py` | 254 | 4, with supervisor worker handoff |
| `attempt_storage.py` | 176 | 4, after cleanup owner replaces its calls |
| **Source subtotal** | **7789** | |

Whole old-subject tests to remove (`tests/` prefix):

| File | Lines | Step |
| --- | ---: | --- |
| `test_dispatch_run.py` | 7255 | 2 |
| `test_memory_admission.py` | 642 | 2 |
| `test_run_invariants_characterisation.py` | 347 | 2 |
| `test_batch_run.py` | 1397 | 3 |
| `test_run_controls.py` | 1348 | 4 |
| `test_context_resume.py` | 199 | 4 |
| `test_context_usage.py` | 363 | 4 |
| `test_worker_completion_guidance.py` | 274 | 4 |
| `test_attempt_evidence.py` | 83 | 1 |
| `test_exec_layout.py` | 158 | 1 |
| **Test subtotal** | **12066** | |

The facade integration step deletes these entire TS files. Fabric owns their
replacement entry/registry glue; execution semantics call run's API.

| File | Lines | Step |
| --- | ---: | --- |
| `runtime/fabric/src/execution.ts` | 895 | 5 |
| `runtime/fabric/src/execution-input.ts` | 610 | 5, route syntax supplied by routing |
| `runtime/fabric/src/batch-execution.ts` | 337 | 5 |
| `runtime/fabric/src/owner-output.ts` | 350 | 5 |
| `runtime/fabric/src/resume.ts` | 349 | 5 |
| `runtime/fabric/src/python-owner.ts` | 49 | 5, after last Python launcher caller goes |
| `runtime/fabric/tests/execution-lifecycle.test.ts` | 2555 | 5 |
| `runtime/fabric/tests/run-closure.test.ts` | 43 | 5 |
| `runtime/fabric/tests/nested-dispatch.test.ts` | 47 | 5 |
| **TS subtotal** | **5235** | |

Retire the inert Rust implementation with the relevant new modules, never use
it as the oracle. Under `tools/front-door/crates/run/src/`: `detached.rs` 403,
`detached_tests.rs` 660, `lease.rs` 603, `lineage.rs` 591, `pending.rs` 299,
`python_unicode.rs` 749 and the old `lib.rs` 7: **3312 lines** replaced.
Paired-primary policy, if retained by workflow, belongs there rather than in
run. None of its Python coercion or differential harness survives.

Shared deletion handoffs below are explicit to avoid double-counting another
area's reduction. The named owner deletes the file with its new implementation
or retirement decision; run removes its calls and contributes the stated
invariants. Counts below are excluded from the run subtotal.

| File or test/helper | Lines | Deletion owner/condition |
| --- | ---: | --- |
| `skills/orchestrate/scripts/cf_dispatch.py` | 969 | Providers/routing; direct entry retired by the plan |
| `skills/orchestrate/scripts/exec_routing.py` | 370 | Routing candidates and quota policy |
| `skills/orchestrate/scripts/fabric_records.py` | 647 | Routing + Fabric: cooldown/health and registry split |
| `scripts/fabric_lanes.py` | 965 | Fabric federation/placement, including operation placement receipts |
| `skills/orchestrate/scripts/run_dir_finalize.py` | 654 | Step 5 with workflow replacing acceptance/retention callers |
| `skills/orchestrate/scripts/run_dir_init.sh` | 138 | Same workflow handoff; no new generic execution scaffold |
| `skills/orchestrate/scripts/lease.py` | 131 | Workflow owns paired-primary policy and replaces/retires its driver lease |
| `tests/test_lease.py` | 56 | Same; excluded from run reduction because the subject belongs to workflow |
| `tests/test_run_dir_finalize.py` | 1112 | Workflow replaces review-plan/panel cases; run replaces closure cases. Do not claim all 1112 as execution tests. |
| `tests/test_fabric_records.py` | 816 | Routing/Fabric replace mixed index, cooldown and health cases |
| `tests/test_cf_dispatch.py` | 2905 | Providers/routing replace external-wire tests with real fixtures |
| `tests/test_cf_dispatch_git_custody.py` | 211 | Confinement/Git contract |
| `tests/cf_dispatch_support.py` | 31 | With the last direct-dispatch test |
| `tests/test_actual_effort.py` | 284 | Providers/routing attribution |
| `tests/support/dispatch.py` | 47 | Last run/conformance/schema test consumer |
| `tests/support/runs.py` | 104 | Same, after schema judge retirement |
| `tests/test_record_schemas.py` | 387 | Plan's old-schema judge retirement |
| `tests/lane_conformance/__init__.py` | 1 | Supervisor + run after R9 custody coverage lands |
| `tests/lane_conformance/conftest.py` | 66 | Same |
| `tests/lane_conformance/fake_provider.py` | 130 | Same; new process fixtures use no Python |
| `tests/lane_conformance/support.py` | 256 | Same |
| `tests/lane_conformance/test_s2_capture.py` | 28 | Same |
| `tests/lane_conformance/test_s2_lifecycle.py` | 185 | Same |
| `tests/lane_conformance/test_s2_rust_faults.py` | 119 | Same |
| `tests/lane_conformance/test_seed.py` | 90 | Same |

Partial edits, not whole-file deletions: front-door `src/main.rs` (dispatch,
batch, run and lane routes), `src/help.rs` and execution help files; Fabric
`server.ts`, `cli.ts`, tool schemas, sessions, lane wait/reader/registry and
surface modules; `scripts/utility.py`, cleanup callers and workflow JS callers;
`skills/orchestrate/SKILL.md`, `references/{chair-loop,worker-liveness,
routing-and-tiers,direct-cli-fallback,memory-scratchpad}.md` and
`skills/implement/references/run-contract.md`. Their owners remove old routes
and contracts in the same rewrite steps. Shared custody, provider execution,
writer leases, record codecs, route tests and Fabric registry tests are not
claimed as run-owned deletions. Session's artifact-cleanup tests keep their
subject until workflow replaces it.

The counted replacement removes **25090 Python/TS lines + 3312 inert Rust
lines = 28402 lines**. Budget **4500 new Rust lines including tests**, for an
expected **net −23902 lines** across Python, TS and Rust, excluding shared
handoffs, prose and caller edits. This is an estimate for review, not a test
or a reason to omit custody coverage. Each build PR measures actual `Net
lines:` once, attributing shared deletions only to the PR that removes them.

## Build order

Each numbered slice is a PR, landing Rust and deleting its old subject and
tests together. These steps begin only after records/config, confinement,
routing/providers/quota and the required supervisor APIs are accepted. No
Python/V8 emulation, selector, bridge or old-output judge is introduced.

1. **Attempt storage and bounded reads.** Add typed records, canonical request
   digests, contained layout and immutable terminal/result publication in
   records/run; test strict versions, tampering and safe output slices. Delete
   `run_records.py`, `layout.py`, `attempt_evidence.py` and their tests with
   state's agreement. Replace the old inert run modules as each new owner
   lands; remove `pending`/Unicode/differential machinery in this slice.
2. **One durable dispatch with admission.** Implement the operation reservation,
   supervisor custody handoff, admission, writer lease and terminal publication
   path. Wire the CLI command directly to Rust. Delete dispatch/preflight/
   memory modules and tests listed for step 2. Port the custody process cases
   first, including disconnected caller and ENOSPC; no live provider needed.
3. **Bounded batches.** Add task-local refusals, writer-scope conflict checks,
   concurrency and aggregate completion from exact attempts. Delete
   `batch_run.py` and its tests. Reuse the dispatch path, not a self-spawned
   command chain; exercise fake providers concurrently.
4. **Controls, continuation and child runs.** Add scoped cancel, bounded wait,
   output, explicit retry/continuation, lineage and slim run close/reconcile.
   Delete the step-4 sources/tests, after supervisor's detached-worker API and
   cleanup's storage replacement land. Prove sibling isolation, exact provider
   session continuation and no duplicate writer after uncertain launch.
5. **Fabric facade and caller cutover.** Fabric replaces the listed TS facade
   and tests with direct run calls, registry repair and reader cursors. Expose
   the command-tree proposal accepted in `design/cli.md`; update skills,
   workflows and hooks whose owners survive. Workflow replaces the shared
   finaliser/scaffold contracts; routing/providers and Fabric remove their
   shared Python files with their own steps. Retire conformance helpers only
   after Rust process coverage exists. Integration tests drive CLI and MCP to
   the same durable attempts, including headless disconnect/restart.

Supervisor/confinement must accept the holder and writer-reservation protocol
before step 2; Fabric must accept event reconciliation and continuation before
step 5. These are owner acceptance conditions,
not permission to ship an unresolved custody path. The chair arranges the
cross-family council and accepts the design; the lane opens a PR and does not
merge or arm auto-merge.
