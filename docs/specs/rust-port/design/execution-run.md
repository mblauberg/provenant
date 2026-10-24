# Execution: runs

Status: proposed for chair acceptance and the command-tree freeze in
`design/cli.md`. Author route: `codex/gpt-6.1-sol@high`, via Fabric.
Governing rules: [PORTING.md v7](../PORTING.md), especially R0, R1 and R9;
dependencies: [ADR 0028](../../../adr/0028-rust-port-dependencies.md), decision 3,
and PORTING R6. The [cross-area decisions](README.md#cross-area-decisions) bind
ownership, layout, commands, waits and telemetry.
Audience: the `provenant-run` build lane and the Fabric, routing and supervisor
owners. Names below are proposed Rust interfaces, not existing APIs.

## Purpose and ownership

`provenant-run` turns a request into bounded provider attempts, retains the
exact prompt, route and result of each attempt, and makes completion and
cancellation reliable across a disconnected caller. It owns admission, batch
scheduling, retries, attempt lineage and run finalisation. Run owns operations,
admission tickets, the run index, attempts, status and event/seen cursors in
`run.sqlite3`, a host-local rusqlite store permitted by R6. Admission and
operation insertion are one transaction. `fabric-runtime` owns identity and
federation placement and routes remote calls; it keeps no second run registry.
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
excluded. The table uses the council
tallies where supplied and labels the remaining initial-sample counts.
Counts establish attempted invocation, not successful execution;
conditional commands inside a Codex wrapper may not have run. The corpus spans
several projects and is a usage sample, not a production counter.

| Recorded surface | Invocations or supplied fields |
| --- | --- |
| `provenant lanes` | Council tally: 696; 22 waits exceeded 3600 s, maximum 14400 s |
| `provenant fabric dispatch` | 31; `--operation-id` 26, model/effort/mode 26 each, worktree 24 |
| `fabric_submit` | Council tally: 1116; no account, intent, risk tier, reviewer, orchestrator family, context ceiling, delivery ID or fallback pool supplied; one fallback list. Confidential true in all 1065 calls containing that field; Codex workers made 15 nested calls. |
| `fabric_run` | Council tally: 254; 89 waits requested 55 s |
| `fabric_output`, `fabric_run` | Initial sample: 31, 16 respectively |
| Old `provenant run` verbs, `provenant batch`, direct `run_controls.py` | None found |
| Direct `dispatch_run.py`, direct `cf_dispatch.py` | 7 and 1 command candidates; not evidence of a required public bypass |

Reproduction locators are kept privately with the council records: one Claude
project transcript with lane waits and Fabric dispatch/cancel on 23 October,
and one Codex session with the direct-dispatch command candidate. These are research locators,
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
| Run-directory scaffolding, `RUN_RECEIPT.json`, `MANIFEST.md`, pane inventory, paired-primary driver lease and manual finaliser | Cut the run-directory contract (about 43 observed executions). Run owns deleting `run_dir_init.sh`, `run_dir_finalize.py`, `lease.py` and their tests with their last callers; run records and guardian scratch settlement replace execution evidence/cleanup; no `clean` verb survives. Workflow keeps delivery/review policy, not these files or a replacement scaffold. |
| `inspect`, `show`, `prompt`, `result`, `diagnostic-log` as separate verbs | Their implementation overlaps status and artifact reads. Consolidate into `lanes` and `output --part`; actual use of a spelling does not require an alias in the new tree. |
| `reduce` and synthetic reduction prompts | No transcript invocation or skill caller was found beyond help/tests. `run_controls._reduce` constructs a special prompt. Dispatch a normal read-only task with explicit retained result references. A batch reports every failed/omitted task; it never implies a successful council. |
| Arbitrary detached commands, PID text markers and `--validate` | `run_worker_detached.py` and Rust `detached.rs` implement a second completion protocol. The facade needs durable provider runs, not an arbitrary argv launcher. Use supervisor-owned workers and typed completion. |
| JSON fallback lists, `fallback:any`, hidden effort downgrade and automatic writer replay | `exec_routing.py` merges several candidate representations; `dispatch_preflight.effort_retry_plan` silently retries rejected effort. Routing supplies a bounded candidate plan. Only opted-in read-only transient failures retry automatically. Every changed route/effort is visible in a new attempt. |
| Runtime reads of Codex session transcripts | `context_usage._codex_rollouts`, `codex_rollout_effort` and `with_codex_rollout` supplement streams from `~/.codex/sessions`. R1 forbids transcript reads by the rewritten runtime. Unknown context or observed effort stays unknown. Recorded, sanitised wire fixtures remain tests. |
| Alias coercion, typo repair, old-root discovery and record migration | `execution-input.ts` repairs route names; controls select newest matching task across old directories. Use strict typed requests and unambiguous IDs. New state starts empty; never scan, migrate or remove `.agent-run/`, `~/.agents` or transcripts. |
| Assurance dispatch flags, `--context-ceiling` and opt-in confidentiality | The council found no assurance/context-ceiling inputs and confidentiality always enabled. Workflow owns assurance policy; routing owns account/eligibility decisions. Confidential is the default; remove the flag and the provider context-ceiling control. Optional `delivery_id` remains a typed workflow correlation. |
| Watchdog silence as an outcome or stop request | Silence is an observation. R5 and lane conformance require a healthy silent provider to survive; only supervisor's closed stop reasons can signal it. |

## Commands and MCP

The binding subtree is top-level `provenant dispatch`, `lanes [--wait SECONDS]`,
`output`, `cancel`, `resume` and `reconcile`. There is no `run`, `fabric run`,
`fabric lanes` or supervisor control subtree. `design/cli.md` freezes flags;
Fabric owns MCP registration/rendering and routes requests to the same run API.
Every command accepts `--json` and the state override from the shared decisions.

| Verb | Inputs and behaviour |
| --- | --- |
| `dispatch` | Exactly one prompt file/stdin input or `--manifest PATH`; required `--operation-id ID`, optional task ID, host and `--delivery-id`. Route/access controls below; `--provider-timeout SECONDS` (the provider runtime budget), `--wait SECONDS`. Manifest submits 1–64 tasks, concurrency default 4, maximum 8. Detached unless `--wait` is given; return exact IDs, route, state and artifact paths. |
| `lanes` | Exact selectors or current actor/project lanes; `--all`, `--limit` and pagination cursor. Read-only unless `--wait SECONDS` is supplied, with `--condition any|all` (default any); `--all` includes retained terminal lanes. No implicit repair. |
| `output` | Exact attempt, `--part result\|prompt\|stderr\|events`, offset (default 0) or tail (1–500 lines, no default), byte cap 1–65536, default 65536. Return offsets, EOF, clipping, encoding and completeness. Prompt reads require dispatch-equivalent authority. |
| `cancel` | Exact run/task/attempt scope, operation ID, reason (maximum 256 bytes) and optional wait. Return requested/already-terminal/unobservable plus custody; accepted cancellation does not prove exit. |
| `resume` | Exact prior attempt, operation ID, `expected_generation` (CLI sends 0) and new prompt. Continued turns preserve route/access/session; a fresh turn or new route is a new `dispatch`. Return a new run and `retry_of`; closed runs stay immutable. |
| `reconcile` | Exact runs (mapped to their operations) or project-scoped recovery scan, and optional typed decision for uncertain work. Recover queued scheduler work, reattach/check holders, finish pending publication and repair summaries/events; never blindly replay an uncertain launch. |

CLI waits take an explicit 1–14400 seconds; MCP 1–55. There is no default,
and zero is not a wait budget. `design/cli.md` owns the exact flags. Fabric's earlier 3600/30 caps are superseded by the shared Waits
row. `wait(Any|All)` snapshots the selected set at entry, excluding caller and
ancestors; later launches belong to the next wait. Any returns on the next
unseen terminal/InputRequired; All returns when all are terminal **or any is
InputRequired**, so attention never waits behind a running sibling. Empty sets
return immediately. Unreachable hosts stay unknown, never completed. Cancelling
a reader ends only its wait. Run stores seen cursors keyed by core `ActorId`
and selection; the edge acknowledges only displayed rows after successful
output. Clipped rows remain unseen. Federated cursors are host-qualified.

Route inputs are exactly one `--model ADAPTER/ID` or `--route NAME`, plus
optional `--effort VALUE`; providers detect accounts, routing filters eligibility
and disclosure. There is no `--account` flag (0 of 1116 dispatches
supplied one); each attempt records its resolved account label. Confidentiality is
always on. Workflow owns assurance
inputs (intent, risk tier, reviewer and orchestrator family), rather than run
flags. Drop `--context-ceiling`. No raw argv, unrestricted knobs, secret bypass
or hidden downgrade. Read-only fallback is explicit
(`ADAPTER/ID[,ADAPTER/ID]`, at most two models; omitted means none).
Access requests carry mode (default read-only), cwd, a required writer worktree,
read roots, network (default off) and capabilities; writes stay in the worktree. Confinement checks caller authority;
flags cannot grant it, and provider access excludes custody/control files.

A manifest is strict JSON `{schema_version:1,tasks:[...]}` with unique task IDs
(1–64 ASCII letters/digits/`._-`, starting alphanumeric), exactly one inline
prompt or prompt-file reference, route/access and optional `delivery_id` per
task. One submission has one host. Duplicate/overlapping writer scopes reject
before launch; task-local refusals do not suppress valid siblings. Manifest and
single prompt limits are 1 MiB, including inline prompts. Snapshot files once
through confinement. `delivery_id` is correlation, never delivery acceptance.
Fabric's proposed concurrency maximum 16 becomes 8: retain the established
4/8/64 defaults absent evidence for raising them.

The edge resolves exact IDs and refuses ambiguous names with candidate IDs.
Human output retains exact route provenance and paths. JSON uses the shared
`{schema_version,command,status,data,error}` envelope and 1 MiB result cap;
execution details live in `data`, never competing top-level fields. Output
byte offsets count original bytes; lossy log text is labelled `utf8-lossy`.
Prompt/result bytes must be UTF-8. Diagnostics are redacted stderr only.
Unknown fields, negative values and range overflow fail validation.

Exit codes follow the shared Command contract: 0 accepted/read/condition met,
1 failed/refused/findings, 2 invalid input/version, 3 setup/I/O/contention,
124 reader deadline, 130 reader cancellation. No exit 4. A satisfied wait,
including InputRequired or unsuccessful terminal outcomes, exits 0 and reports
outcomes/partial coverage in `data`; explicitly assessing a failed run exits 1.
A provider timeout is recorded as `timed_out`, distinct from reader expiry.

## Records, layout and publication

`StateRoot` comes from `design/state.md`; this area does not pick a second
home or fall back to an old root. All paths below are relative to that fresh
root. Run IDs are opaque core IDs, not timestamp suffixes or PID aliases.

```text
run.sqlite3                    # operations, admission, runs/attempts, status, events/cursors
run.sqlite3-wal / -shm          # SQLite side files, same private directory
runs/<RunId>/                  # ProjectId is recorded, not a parent directory
  request.json                 # immutable canonical submission/prompt digests
  owner.json                   # supervisor-issued scheduler custody reference
  custody.lock                 # supervisor-owned run custody
  children/{journal.lock,<ChildId>.json} # supervisor child-custody evidence
  attempts/<AttemptId>/         # task association/ordinal live in the run store
    prompt.txt                 # run-owned immutable exact provider prompt
    result.md                  # run-owned sealed result, including failure
    custody.lock               # supervisor-owned attempt custody
    launch.json                # supervisor-owned redacted launch summary
    process.json               # supervisor-owned replaceable observation
    cancel.json                # sole supervisor-consumed cancellation request
    process-exit.json          # sole supervisor exit/cleanup evidence
    stdout.log                 # sole bounded/redacted stdout wire capture
    stderr.log                 # sole bounded/redacted stderr capture
    reserve                    # supervisor's 64 KiB terminal-publication reserve
    provider-state/            # retained provider state/history; no credentials
    quarantine/                # retained patches/blobs and manifest
    scratch/                   # disposable launch staging only
```

This shared attempt path and these supervisor filenames are the run/supervisor
contract, under the binding top-level Layout row; there is no
`control.json`, `supervisor.json` or second events capture. Provider parsing
uses the live stream, not a second on-disk copy. Structured run events live
in SQLite. Retention policy is out of scope. Guardian settlement deletes only
disposable `scratch/` data; history, unclassified data and transcripts remain
retained, never staged there.
Modes are 0700/0600 under state's verified root. Use rusqlite WAL,
foreign keys and `synchronous=FULL`; each connection has one owning thread,
bounded BUSY retries and no transaction spanning a wait, resource probe or
supervisor call. R6 explicitly permits this second database alongside Fabric's
coordination store. Root/path safety and file publication use supervisor APIs.

Run owns schema and transactions; shared typed records live in records:

- `RunRecord`: schema version, run/project IDs, core actor, operation ID,
  immutable request digest, task IDs, parent/children, revision/generation,
  created/closed times and closure state. The run index is in this same store.
- `AttemptRecord`: IDs, optional `delivery_id`, retry/continuation parent,
  requested/resolved/applied/observed route/effort, confinement and subject
  identity (`GitOid` supports SHA-1/SHA-256), admission/runtime deadlines,
  custody/session, timestamps, state/outcome/reason, usage/context with source,
  reviewer provenance when supplied by verified edge evidence, artifact
  paths/digests, clipping/redaction and publication errors.
- `AttemptFact`: `Serialize`-only terminal projection in records, built by run
  from validated store records: task, attempt number, operation, project,
  terminal state, exit, `started_at`/`finished_at`, resolved route (provider,
  model, effort, account label), usage counts (`u64`), output paths, reviewer
  and optional `delivery_id`. Timing adds `queued_at` (admission accepted),
  optional `started_at` (ticket granted) and optional `provider_ms`
  (supervisor-observed execution) for telemetry's comparison view. It also
  carries workflow's subject/head, output references/digests,
  requested/applied/observed route, verified owner, lineage and reason,
  covering every execution field read from `RUN_RECEIPT`.
- `OperationRecord`: project/actor scope, kind, request digest, IDs and
  `prepared|accepted|terminal|launch_unknown`. The digest excludes reader
  wait/rendering but includes prompt, route, authority and execution controls.
- `AdmissionTicket`: operation/attempt/project IDs, sequence, holder custody,
  active/queued count reservation, memory/disk floors, startup spacing,
  quota permit reference, separate admission deadline and resource samples.
  A control receipt references `cancel.json`; it is not a second cancel file.
- `RunEvent` and cursor rows: monotonic host sequence, project/attempt/revision,
  kind and typed change. Events and index changes commit with the attempt;
  per-actor acknowledgements and cursor scope are durable in this store.

Stored records use `schema_version:1`, strict fields and tagged enums.
Edge-built facts, including `AttemptFact`, implement `Serialize` only, not
`Deserialize`; readers validate stored records before constructing facts. Operation
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
Terminal records are immutable. The latest attempt for a task is the one with
the highest attempt number, never the latest timestamp or record revision.

Publication order is load-bearing:

1. In one SQLite transaction reserve operation/request digest, run/attempt IDs
   and admission tickets/counts. Same scoped key/digest returns the receipt;
   a different request is `operation_conflict`. Reject queue overflow before
   acceptance. Publish the immutable request before acknowledging it.
2. The scheduler samples resources outside the transaction, rechecks capacity
   while claiming a ticket, acquires writer custody and hands supervisor the
   immutable plan/publication reservation. A lost acknowledgement records
   `launch_unknown`; capacity and writer reservations remain held.
3. Seal/fsync/hash result and supervisor exit evidence first. Commit immutable
   terminal AttemptRecord after observed exit/cleanup, run summary, operation
   state, released admission
   capacity and revisioned event together. Capture bytes are never completion
   evidence. File publication uses supervisor's atomic, directory-fsynced API.
4. Run serves queries/snapshots/events from its store. Recovery retries a
   pending terminal commit from sealed artifacts and supervisor facts, keyed
   by attempt/revision, without replacing an already committed outcome.
5. Close only after all tasks and registered children are terminal. Record any
   authorised child ownership transfer. Closure does not prune evidence,
   clean worktrees or decide delivery acceptance.

There is one publication writer per run. Concurrent submissions/control changes
use bounded store transactions; no provider launch occurs inside one. Temporary
file/DB crash gaps are explicit prepared/publication-pending states reconciled
from durable artifacts, never interpreted as successful launch/completion.

Reserve terminal-publication capacity before launching. ENOSPC, capture faults,
a dropped observer and index/telemetry write failures never create a stop
reason. Supervisor retains custody and raw terminal facts until durable
publication is acknowledged; after a transient write failure, retry from those
facts. Persistent failure remains `settling` with `publication_pending:true`; custody
and writer reservations remain held for safe recovery. Never report success or
release a writer early to make a bookkeeping failure disappear. No design can
promise successful disk publication during permanent storage failure.

## Library interfaces and dependencies

The shared API below matches Fabric's names; shared value/request/result types
live in core/records, never Fabric-private copies. `ExecutionContext` contains
ProjectId, core `ActorId`, host, authority and verified parent reference. A
`RunStore` handle is opened for that context; project-only methods below verify
it. `submit`, `cancel`, `resume`, `read_output` and `export` additionally take
`&ExecutionContext`; `recover`, `reconcile` and cursor acknowledgements check
the handle's bound project/actor/authority before mutation.
`CancelToken` and `WaitBudget` come from core; every blocking operation has a
budget. `RunStore::open(&ExecutionContext, WaitBudget, &CancelToken)` binds
authority and a call budget; later calls refresh it explicitly. A handle never
retains a transaction between calls.

| Owner / caller | API and contract |
| --- | --- |
| run / CLI, Fabric | `submit(Submission, OperationKey, &ExecutionContext) -> Result<SubmissionReceipt, RunError>`; Submission has 1–64 tasks and concurrency 1–8. Single dispatch and batch share this path. |
| run / CLI, Fabric | `operation(ProjectId, OperationId)` and `recover(ProjectId, OperationId) -> Result<Observed<SubmissionReceipt>, RunError>`; `reconcile(ProjectId, OperationId, ReconcileDecision) -> Result<RunSnapshot, RunError>`. Recover resumes known queued/publication work; reconcile checks uncertain custody before an explicit decision. No implicit writer replay. |
| run / CLI, Fabric | `query(RunQuery) -> Result<Page<RunSnapshot>, RunError>`; `snapshot(ProjectId, AttemptRef) -> Result<RunSnapshot, RunError>` reports the active count for its selection, including starting and custody-unknown reservations; `wait(RunQuery, WaitBudget, &CancelToken) -> Result<Page<RunSnapshot>, RunError>`, with `WaitCondition::Any\|All` and the attention semantics above. |
| run / Fabric federation | `events(RunCursor, usize) -> Result<Page<RunEvent>, RunError>` and `ack_events(RunCursor)` on the scoped handle; lazy pulls, durable per-host cursors, no Fabric registry. |
| run / CLI, Fabric | `cancel(AttemptRef, OperationKey, CancelReason, &ExecutionContext) -> Result<CancelReceipt, RunError>`; aggregate scopes expand to exact attempts. `resume(ResumeRequest, OperationKey, &ExecutionContext) -> Result<SubmissionReceipt, RunError>`; request includes `expected_generation`, exact previous attempt and prompt; it cannot change route or access. |
| run / CLI, Fabric | `read_output(AttemptRef, ArtifactPart, ByteRange, &ExecutionContext) -> Result<OutputChunk, RunError>`; `export(AttemptRef, ExportLimits, &ExecutionContext) -> Result<ArtifactManifest, RunError>`. Bounded confined regular files and stable digests. |
| run / Fabric, confinement edge | `capabilities() -> Result<ExecutionCapabilities, RunError>`; `temporary_paths(ProjectId, AttemptRef) -> Result<AttemptTempPaths, RunError>`. Read-only host-local observations/paths, checked against handle authority. |
| run / workflow composition edge | `AttemptReader::attempt(AttemptId) -> Result<AttemptFact, RunError>` and `attempts(filter)` returns the shared terminal fact; nonterminal attempts are a typed error. `work_snapshot(WorkScope) -> Result<RunWorkSnapshot, RunError>` carries generation, observed time, active count/attempts and held leases. |
| run / CLI composition edge | `AttemptReader::attempts(filter) -> impl Iterator<Item = Result<AttemptFact>>` lazily yields validated terminal facts from the scoped store with bounded pages; read/validation failures are surfaced. The CLI passes facts to telemetry's `stats(facts, &Acceptance, &PriceTable, window) -> Stats`, which accepts any iterator of `AttemptFact`. |
| routing / run | `resolve(&RouteRequest, &RoutingContext) -> Result<ResolvedRoute, RoutingError>`; optional bounded plan of at most two alternatives, applied effort, eligibility, readiness and confidentiality. No candidate reconstruction in run. |
| quota / run | `admit(&ResolvedRoute, &OperationId) -> Result<AdmissionPermit, QuotaError>`; provider/account quota readiness only. Run records the permit with its capacity/resource reservation, consumed by that exact attempt; quota owns no second host admission ledger. |
| providers / run | `prepare(PrepareInput) -> LaunchSpec`; incremental `observe(bytes) -> ProviderEvent`; `finish(ExitObservation, artifacts) -> ProviderOutcome`; continuation capability and exact session token. No context-ceiling control or transcript reads. |
| supervisor / run | `launch(OwnedLaunch, CapturePlan, PublicationReservation) -> CustodyHandle`; `observe(handle, WaitBudget, &CancelToken) -> Observation`; `stop(handle, StopReason) -> StopReceipt`; `recover(CustodyRef) -> Reattached\|Exited\|Unknown`; same-binary detached worker handoff. Safe filesystem publication/locks have explicit budgets; fixed Git evidence reads use `run_utility`. |
| confinement, config / run | Validated prompt snapshots, canonical writer scopes, confined plans and lifetime-bound writer reservation; reject primary-checkout writers, secrets and unsupported enforcement. Config supplies immutable policy snapshots. Resource/liveness probes use supervisor's safe OS API. |

`RunError` distinguishes validation, refusal, contention, cancellation, version,
unsupported continuation, launch uncertainty, publication and I/O with causes
and paths. Provider failures are outcomes. No library renders exits or imports
cli. Fabric supplies identity/placement, remote operation uncertainty and
session alias updates from returned receipts; it never relocates an uncertain
writer. Workflow receives edge-built facts for every execution field it read
from `RUN_RECEIPT`; review plans, pane inventory and paired-primary policy stay
workflow concerns, not provider assertions or a new run-directory contract.

The CLI composes the reader and pure stats reducer; run does not depend on
telemetry. Telemetry owns the price schema (`PriceTable`); state owns loading
bundled TOML assets. Stats filter on `finished_at`, with no revision dedupe:
terminal records never change. There is no ledger or import. Fabric uses the
wait caps, concurrency and exit envelope from the shared decisions.

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

Admission uses one store transaction for counts/tickets plus resource samples,
not directory scans or a second ledger. Hard limits are 64 active attempts per
host, 16 per project/host and 256 queued tasks per host across all entry points.
Starting and custody-unknown attempts retain capacity until reconciled. Dead
schedulers do not discard accepted queue work: `reconcile` claims their tickets
with a new generation after supervisor verification. Defaults are five per cent
available memory for readers, ten for writers and 500 MiB disk headroom for either. Retain a
20-second startup spacing window to avoid simultaneous memory spikes. A
failed probe or unavailable lock queues until the request's admission deadline,
then refuses, rather than launching unchecked. Admission has its own deadline
(default 3600 seconds read-only, 10800 writer; not caller-set). The provider runtime budget retains the old defaults
3600/10800 seconds and counts provider execution across automatic retries only;
queueing, routing and admission consume none of it. Record each duration.
Batch concurrency remains 4/8 with 64 tasks, distinct from host admission caps.
No live provider is stopped because a later memory or disk sample falls below a threshold.

Continuation allocates a new RunId, with `retry_of` pointing to the exact old
AttemptRef; a manual retry is a new `dispatch`. Closed runs never reopen. Resume atomically compares/advances a separate
per-target continuation generation with the new operation; replay of the same
operation returns its receipt before that check. The old terminal attempt is
unchanged. Automatic read-only
fallback alone appends an attempt inside the original run/task. All retries
require observed prior exit and completed cleanup.
On macOS, completed cleanup means a reaped provider, two complete libproc
censuses at least 20 ms apart with no provider session/group member or surviving
recorded descendant/ancestry (ppid with native start-time identity), and fresh
nonblocking writer-lease acquisition after all guardian/requester copies close.
Libproc enumeration supplements permission-limited short BSD records with
the public `KERN_PROC_PID` start-time prefix, checked across two native reads.
Failed or racing identity reads remain unknown. The provider inherits that
lease description. Supervisor records
`darwin_census_and_custody_lease` settlement evidence; any missing proof stays
fenced. An unsampled `setsid` escape that closed the lease is the documented
macOS residual. Run never converts an uncertain observation into absence.
 Continued resume
preserves route, account, confinement and exact provider session; unsupported
or missing sessions fail without a fresh launch. A new route or fresh session
is an explicit new dispatch. Replaying a successful attempt without new input is refused.
Input-required continuation retains the question and answer in
its new prompt. Opted-in read-only fallback follows only typed transient
rate/quota/model-unavailable outcomes, within the original budget and at most
three attempts. Authentication, permission denial, quarantine, partial output
and unknown custody never trigger automatic retry. Writers require an explicit
retry even after a transient failure, with fresh worktree evidence.

Run produces the supervisor release contract after committing the canonical
terminal transaction and quarantine. In Run-private directories excluded from
provider mutation, durably publish v1 `terminal-commit.json` and
`quarantine/manifest.json`, each containing `schema_version: 1`, exact `scope`
(run, attempt, generation), and `kind` (`canonical_terminal` or
`quarantine_manifest`). Then publish `release.json` with the same scope/version
and `terminal_digest`/`quarantine_digest`, calculated as
`CanonicalV1::file_digest` over the exact raw bytes. Each record is at most
1 MiB and rejects duplicate/unknown fields. Supervisor verifies the original
attempt inode and held custody lock, both digests and scoped records, and syncs
the held files and directories before issuing `SettlementCommit`. Run retains
writer exclusion through classified scratch settlement and fence clearing;
missing, malformed or changed attestations keep the fence.

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
- Every test sets temporary `HOME`, state root and all harness-config paths;
  the fake provider executable and temporary repositories use no real install,
  credentials, old state or harness settings.
- Binary integration tests drive the real command tree and MCP facade
  against that isolated environment. Cover both
  access modes, manifest rejection before launch, bounded concurrency,
  atomic admission/operation rollback, host/project/queue caps, independent
  admission/runtime deadlines, operation replay, per-attempt result paths,
  fake-secret redaction across chunks,
  pagination/offsets, UTF-8 cuts,
  wait Any/All (including InputRequired with a running sibling), 14400/55
  boundary validation, event replay/ack cursors, refusal/exit codes,
  expected-generation conflicts, terminal fact fields/immutability, latest by
  attempt number, snapshot active counts, `finished_at` filtering, completed
  facts/readers, continuation and changed
  route receipts. Place hostile sentinel old roots beside the new root and
  prove no reads, writes or migration occur.
- Process tests run on Linux and macOS with FIFO/handshake synchronisation:
  immediate child exit after observation registration; silent healthy provider;
  reader/MCP death; scheduler death with surviving holder and queued tasks,
  followed by explicit `reconcile` with no duplicate acceptance; holder death with
  reattach/unknown and writer-relaunch refusal; observer death; publication faults
  including ENOSPC then recovery; one writer across independent callers;
  lease held through cleanup; queued and running cancellation; natural exit
  racing cancel; grandchild cleanup without stopping a sibling/shared chair;
  concurrent child registration and parent closure; remote uncertainty with no
  duplicate writer launch. Assert durable terminal facts/results before reaping.
- Use an in-house snapshot helper (about 80 lines), not `insta`.
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
above. This is an implementation deletion plan; this design PR changes this
document and PORTING R2's run dependency row. A mixed helper is deleted only
after each surviving concern has its Rust owner. Do not retain a Python/TS adapter while waiting for another
owner; delete old-subject tests and update surviving callers as R10 requires.

Whole run sources to remove (`skills/orchestrate/scripts/` prefix):

| File | Lines | Step |
| --- | ---: | --- |
| `dispatch_run.py` | 2483 | 2+3, with batch importer |
| `dispatch_preflight.py` | 584 | 2+3, with dispatch/batch importers |
| `memory_admission.py` | 340 | 2+3, with dispatch importer |
| `batch_run.py` | 933 | 3 |
| `run_controls.py` | 1192 | 5, with `run_dir_finalize` and Fabric callers |
| `run_lineage.py` | 656 | 4, after provider_exec and run callers |
| `run_records.py` | 455 | 5, last importer |
| `layout.py` | 245 | 5, last importer with state/confinement |
| `attempt_evidence.py` | 107 | 5, after run-directory cut and run_records |
| `context_usage.py` | 364 | 5, after provider/Fabric importers retire |
| `run_worker_detached.py` | 254 | 4, with supervisor worker handoff |
| **Source subtotal** | **7613** | |

Whole old-subject tests to remove (`tests/` prefix):

| File | Lines | Step |
| --- | ---: | --- |
| `test_dispatch_run.py` | 7255 | 2 |
| `test_memory_admission.py` | 642 | 2 |
| `test_run_invariants_characterisation.py` | 347 | 2 |
| `test_batch_run.py` | 1397 | 3 |
| `test_run_controls.py` | 1348 | 5 |
| `test_context_resume.py` | 199 | 5, with controls |
| `test_context_usage.py` | 363 | 5, with last-importer deletion |
| `test_worker_completion_guidance.py` | 274 | 4 |
| `test_attempt_evidence.py` | 83 | 5 |
| `test_exec_layout.py` | 158 | 5 |
| **Test subtotal** | **12066** | |

Fabric owns deletion of these entire TS files at step 5, including their
replacement entry/federation glue. These **5235 lines are excluded** from run's
reduction; Fabric counts them once. Execution semantics call run's API.

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

Additional ownership/deletion handoffs below are excluded from the conservative
run estimate, including the newly run-owned run-directory/Git evidence cuts.
The named owner alone deletes/counts each file when its last importer retires;
workflow and supervisor do not also claim run-directory or Git packet files.

| File or test/helper | Lines | Deletion owner/condition |
| --- | ---: | --- |
| `skills/orchestrate/scripts/cf_dispatch.py` | 969 | Providers/routing; direct entry retired by the plan |
| `skills/orchestrate/scripts/exec_routing.py` | 370 | Routing candidates and quota policy |
| `skills/orchestrate/scripts/fabric_records.py` | 647 | Routing/Fabric retire mixed Python; run owns the new index |
| `scripts/fabric_lanes.py` | 965 | Fabric federation/placement, including operation placement receipts |
| `skills/orchestrate/scripts/run_dir_finalize.py` | 654 | Run, step 5: cut contract after workflow/orchestrate callers change |
| `skills/orchestrate/scripts/run_dir_init.sh` | 138 | Run, step 5: remove scaffold and its layout CLI call |
| `skills/orchestrate/scripts/lease.py` | 131 | Run, step 5: retire driver lease with run-directory cut; distinct from writer custody |
| `tests/test_lease.py` | 56 | Run, step 5; delete inert lease/oracle consumers too |
| `tests/test_run_dir_finalize.py` | 1112 | Run, step 5: delete old subject, retain Rust closure/custody coverage and workflow policy tests |
| `skills/orchestrate/scripts/git_evidence_packet.py` | 88 | Run, step 5 with last dispatch/control and Rust-oracle importer; Git reads use supervisor run_utility |
| `skills/orchestrate/scripts/attempt_storage.py` | 176 | Supervisor alone counts/deletes it with last provider/cleanup caller, no run claim |
| `skills/_shared/roots.py`, `scripts/lib/roots.py`, `scripts/host_identity.py`, `skills/_shared/harness_profile.py`, `scripts/harness_profile.py` | Not counted here | State/config counts them; delete with each last importer in step 5, after providers/routing, workflow, Fabric and CLI retire their old consumers |
| `tests/test_install_skills.py` | 1311 | State deletes the whole old-installer test; no run edit or count claim |
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

Surviving callers and test infrastructure have explicit owners below. Steps
refer to this run plan; an owning area's earlier slice may satisfy the gate.
Delete old-subject cases and fixtures; edit mixed files for their surviving
subject. None of these partial edits is included in the reduction estimate.

| Caller / fixture | Owner and run-step gate |
| --- | --- |
| `tools/front-door/src/{main.rs,help.rs}`, execution help files, `tools/front-door/tests/common/mod.rs` | CLI, steps 1/2/4: native test rig and frozen verbs; remove old routes/oracle hooks |
| `config/harness-knobs.json:19,25`, `tests/test_harness_profile.py` | State/config, step 2: replace knobs with typed config; remove retired context-ceiling settings |
| `scripts/clean.py:233` | Supervisor S3 deletes `clean.py` with its tests; guardian scratch settlement and `worktree remove --merged` replace it. No `clean` verb or run-directory contract survives |
| `tests/conftest.py` | Run + CLI + Fabric, each step: remove deleted fixtures; temporary HOME/state/harness paths |
| `tests/test_model_route_pools.py` | Routing, step 2: replace obsolete dispatch/route assumptions |
| `tests/test_issue_697_custody.py` | Run + supervisor, steps 2/4: replace old subject with native custody/fault cases |
| `tests/test_ci_repository_assurance_policy.py:319` | Workflow/CI, step 5: update path policy with the run-directory/shared-leaf cuts |
| `tests/golden/cases/dispatch`, `tests/golden/cases/batch`, `tests/golden/cases/run` | CLI, steps 2/3/4: delete old judges with subjects, test new contract directly |
| `runtime/fabric/src/{server.ts,cli.ts}`, schemas, sessions, lane wait/readers and surface modules | Fabric, step 5: direct run API calls; remove local registry/seen-cursor ownership |
| `runtime/fabric/tests/execution-owner-fixture.mjs`, `lifecycle-owner-fixture.mjs`, `v2-owner-fixture.mjs` | Fabric, step 5: delete after last old-owner test |
| `runtime/fabric/tests/execution-pruning.test.ts`, `graph-runner.test.ts`, `lean-surface.test.ts`, `named-sessions.test.ts`, `surface.test.ts` | Fabric, step 5: remove old cases; preserve fresh Rust facade/session/cancellation coverage |
| `scripts/utility.py` | Execution-route deletes it, `test_utility.py` and `utility-schema.cjs` in route step 3 with `task_classifier`'s utility calls, gated on their last importer; CLI removes utility verbs/hook. No run deletion/count claim. |
| Workflow JS run-directory callers | Workflow, step 5: remove provider_exec.layout/scaffold use; no adapter |
| `skills/orchestrate/SKILL.md`, `references/chair-loop.md`, `worker-liveness.md`, `direct-cli-fallback.md:8`, `memory-scratchpad.md:16` | Run, steps 2–5: frozen commands, bounded waits and typed evidence instead of receipts/scaffold |
| `skills/implement/references/run-contract.md`, `skills/autopilot/references/codex-operator.md:43` | Workflow, step 5: consume completed facts and update command examples |

The counted run replacement excludes Fabric's 5235 lines and supervisor's
176 lines. It removes **19679 Python lines + 3312 inert Rust lines = 22991 lines**.
Budget **4500 new Rust lines including tests**, for an expected **net −18491
lines**; 5411 lines belong to the other owners. Additional
run-owned cuts above are deliberately unbudgeted; each build PR reports actual
`Net lines:` once. Size never justifies omitting custody coverage.

## Build order

The numbered milestones land in four PRs: 1, 2+3, 4 and 5.
Each group lands Rust and deletes its old subjects/tests together; no group
lands an intermediate commit that removes a still-imported module. These steps
begin only after records/config, confinement,
routing/providers/quota and the required supervisor APIs are accepted. No
Python/V8 emulation, selector, bridge or old-output judge is introduced.

1. **Store and bounded reads.** Implement run.sqlite3 schema/typed records,
   canonical requests, contained artifacts, terminal publication and read
   bounds. Replace inert run modules as their owners land; delete pending,
   Unicode and differential machinery. Do not delete shared Python leaves
   while old importers remain; they implement only the old subject, never a
   Rust/Python adapter. CLI's native isolated rig lands here.
2. **Durable dispatch and admission.** Implement atomic operation/tickets,
   count/resource admission, separate deadlines, supervisor handoff, writer
   reservation and terminal publication. Wire top-level dispatch to Rust;
   delete dispatch/preflight/memory modules and tests with milestone 3, whose
   batch_run imports dispatch_run and dispatch_preflight. Land custody/ENOSPC
   cases first. Routing/providers/config replace their dispatch callers.
3. **Bounded batches.** Implement manifest submission, task-local refusals,
   scope conflicts, concurrency and aggregate results through the same submit
   path. Delete batch_run and its tests. Use concurrent fake providers.
4. **Controls and recovery library.** Implement scoped cancellation,
   query/wait/output, expected-generation resume, lineage, work facts and
   explicit reconcile. Delete run_lineage and run_worker_detached and their
   tests after worker/cleanup APIs land and provider_exec importers retire.
   Prove sibling isolation, exact continuation and no duplicate writer after
   uncertainty. This PR completes the typed run API without Fabric glue.
   Keep run_controls solely for its old Fabric/finaliser callers until step 5;
   it is never called by Rust or adapted to the new store.
5. **Facade, run-directory cut and shared leaves.** Fabric replaces/counts its
   TS facade and mixed tests, routing remote queries/events to run; workflow
   consumes `AttemptReader::attempt` at the composition edge (workflow does not use `work_snapshot`). The seam
   is the tested library from step 4: this PR adds federation/MCP composition
   and removes the remaining old consumers together. Delete run_controls and
   its tests with `run_dir_finalize` and the Fabric execution/batch/resume
   callers. Run cuts run_dir_init, RUN_RECEIPT/MANIFEST and the driver lease
   after the caller matrix is updated; run records plus guardian scratch settlement replace them.
   Delete git_evidence_packet with its last control and inert workflow oracle
   consumers; Git reads use supervisor run_utility. Update lane_launch's
   dynamic run_records cancellation call. Finish these last-importer groups:

   | Leaf (run deletion owner) | Last-importer gate |
   | --- | --- |
   | `run_records.py` | dispatch/preflight (2), batch (3), controls (5), lane_launch dynamic call and all old tests/oracles gone |
   | `attempt_evidence.py` | run_records above, dispatch/batch/controls and `run_dir_finalize` (5), records/workflow Python oracle references gone |
   | `layout.py` | dispatch/preflight/batch/controls, provider_exec, provider_confinement, fabric_records, utility.py and run_dir_init.sh all removed/replaced by their owners |
   | `context_usage.py` | dispatch_run, provider_exec, provider_observer and fabric_records all removed/replaced by providers/routing/Fabric; then delete context usage tests |

   Supervisor alone deletes attempt_storage with its last provider/cleanup
   importer. State/config deletes roots, host_identity and harness_profile with
   each last importer in step 5, not when its replacement first lands.
   Routing/Fabric retire fabric_records with the mixed health/index
   tests. Retire conformance/support fixtures only after native custody tests
   replace them. CLI/MCP tests exercise one durable store through disconnect,
   scheduler restart, explicit reconcile and event cursor recovery.

### Decisions taken in steps 2+3

The chair accepted the same-binary guardian as holder: it keeps custody until
Run's terminal commit is durable, and restart recovery is `reconcile`, with no
scheduler daemon. Within that, the lane decided:

- **Admission runs in each attempt's guardian**, through `Holder::admit`
  before spawn, like the old per-task waiter. Caps are 64 active per host, 16
  per project, the run's concurrency and 256 queued per host (checked at
  submit). Floors are 20 s host start spacing, 5% free memory and 500 MiB of
  disk. The admission deadline is 3600 s plus one provider timeout for each
  earlier wave of the task's own run, so a deep batch at low concurrency is not
  refused. A failed probe or a busy store keeps the attempt waiting. Refusal or
  expiry publishes a never-spawned exit, which becomes `rejected`. Guardians
  poll independently with backoff capped at 2 s, so admission is roughly but
  not strictly first come, first served; a strict queue would let one dead
  guardian block its project until its deadline.
- **Attempt flow** is queued, then starting (generation 1, applied route,
  before handoff), then running (admitted), then terminal. A prepare refusal
  becomes `rejected` with no custody and no exit evidence, as does a local
  failure before anything reaches a guardian. A handoff that does not confirm
  marks the operation `launch_unknown` and leaves the attempt starting for
  `reconcile`. The guardian retries the terminal commit for about 30 s;
  `reconcile` finishes anything still held after that.
- **The holder decodes the sealed `stdout.log` after exit.** Success requires
  exit 0, a complete non-empty result and an observed route that verifies.
  A wall-deadline stop becomes `timed_out` and an explicit cancellation becomes
  `cancelled`, unless the provider had already succeeded. Redaction is
  deferred: no secret reaches the provider environment yet.
- **Worktree-write dispatch** accepts an absolute path or a path relative to
  the selected repository's primary root. The edge verifies a linked worktree
  of that project and its active Provenant registration, then records the
  canonical writer scope in the immutable task request. Run acquires the
  worktree's supervisor reservation and exclusive lease before compiling the
  confinement plan, and transfers custody to the guardian. A second writer is
  refused. The guardian releases the reservation after terminal publication
  and settlement. Network, capability, read-root and remote-host requests
  remain unsupported.
- **Writer terminal checks** reverify the recorded worktree and call
  `check_writer_terminal` before committing an outcome, including recovery
  through reconcile. An unfinished cherry-pick, revert, rebase, merge or
  sequencer operation produces `failed` with the worktree and reason, even
  when the provider returned a complete success and Git reports a clean tree.
- **Route fallback is applied at prepare time** across the resolved route's
  alternatives. Runtime fallback on rate, quota or model-unavailable is
  deferred, because no decoder emits those events yet.
- **The provider seam is minimal**: `Providers::prepare` returns argv,
  allow-listed environment, stdin, cwd and applied route, and `decode` maps a
  sealed capture to an outcome. It is shaped for the providers crate's launch
  specification. The composition edge prepares activated Codex launch specs;
  Claude still needs a credential-authority binding. Provider execution does
  not waive the existing terminal requirement for an observed route: Codex
  streams without an answering-model observation cannot establish success.
- **The guardian re-resolves the state root** from a `RootSeed` in the holder
  context, because its environment is cleared.

### Decisions taken in steps 4+5

- **Cancel** ends queued work at once as `cancelled`, with the caller's reason.
  Started work gets a supervisor stop request, and its guardian maps the exit
  to `cancelled`; a guardian still waiting for admission sees `cancel.json`,
  publishes a never-spawned exit and the attempt becomes `cancelled`. The
  receipt says requested, not exited. An attempt still mid-handoff is retried
  for half the call budget; if its launch summary never appears it is reported
  as unobservable and the command fails, so the caller retries or reconciles.
  One attempt's failure never hides the other receipts. The operation ID is
  bound to its target in `cancellations` (schema version 2, added to a
  version 1 store on open) before anything is touched: the same
  ID and target is a replay, another target is an operation conflict. Fabric
  reports an unobservable cancel as `launch_unknown` with its operation ID,
  locally and through a peer.
- **Resume** creates a new single-task run whose attempt is the next attempt
  number of the prior task, with `retry_of` set and route, access and timeout
  copied from it. Each terminal attempt can be continued once: a
  `continuations` table enforces it and the CLI sends expected generation 0.
  The provider seam receives the prior attempt and must continue its exact
  session. No provider exposes sessions yet, so the CLI refuses `resume` as
  unsupported before it spends the continuation, and production `prepare`
  refuses any continuation that still reaches it. A replay compares the
  rebuilt request digest, so a changed prompt under the same operation ID is
  an operation conflict.
- **Reconcile** observes each nonterminal attempt through supervisor custody.
  A durable exit is committed through the same `finish` path as the guardian
  and its settlement published; a starting attempt without a launch summary,
  whose reservation is provably absent and which has no exit, is withdrawn as
  `rejected`; queued work is started; everything else stays fenced and is
  reported as held or uncertain. Accepting lost custody
  (`--abandon-uncertain`) remains unavailable at the CLI. Writer reservations
  survive owner death, including death after terminal commit, and reconcile
  includes their run IDs and exact reservation paths in `writer_reservations`.
  A starting writer with a never-handed-off `Prepared` reservation is safely
  withdrawn under its worktree lock. Reconcile also clears that preparation
  fence after a terminal withdrawal that proves no provider launched and has
  no exit evidence. Other reservations without a launch
  summary stay uncertain; absence of an attempt-local reader reservation
  cannot withdraw them. A retained
  reservation after a provider's terminal commit also stays uncertain. The chair recovery
  procedure is in [worktrees](../../../worktrees.md#writer-custody-recovery).
  Reader fences on terminal attempts stay, because attempt directories are
  never reused. A per-attempt or per-run error is reported in
  place and the scan continues; anything left uncertain, unlaunched or in
  error fails the command. The scan covers the 320 oldest open runs and says
  when more exist; `--id` reaches the rest.
- **Telemetry stats** reads this project's terminal attempt facts and prices
  them from the reviewed `config/telemetry-prices.toml` product asset.
  Cross-project totals are out of scope for this slice.
- **Publication order.** A terminal commit seals `result.md` first and then
  `terminal-pending.json`, so a sealed intent always names a sealed result. A
  result without intent goes through `recover_result_only`; a differing
  intent or result is refused as needing recovery. A withdrawal interrupted
  after sealing its intent resumes that intent's outcome, time and reason, so
  a retry never writes a second set of facts.
- **Replay.** A crash before an operation is accepted leaves it prepared under
  its first IDs. A replay with the same request digest adopts those IDs
  instead of reporting a conflict.
- **Reservation race.** Reconcile withdraws a start without a launch summary
  only while holding the writer lock: either supervisor's `prove_unreserved`
  (no reservation, launch or exit) or the recovery lease. The launcher
  re-reads the attempt after taking custody and stays local if it
  changed, so a withdrawn attempt never reaches a guardian.
- **Input scan.** `dispatch`, `resume` and Fabric submissions scan each prompt
  against confinement's secret patterns before any record is written and
  refuse with `SECRET_INPUT`, without echoing the match.
- **Worktrees.** A missing local worktree is `WORKTREE_MISSING` (Fabric
  `worktree_missing`, #1309); a writer without a worktree or a reader with
  one is invalid (#1310). Writer scope is persisted and reverified at launch
  and terminal publication; primary, foreign and unregistered trees refuse.
- **Fabric's facade** calls the CLI's shared `submit`, `continue_attempt`,
  cancel and output paths, so Fabric submissions, graph nodes and the CLI
  admit, record and launch identically. Output fetch accepts only the
  attempt's bound result digest and returns the whole result, paging the
  64 KiB reader, or refuses with `output_too_large`. Remote runs forward to the owning host. Run
  listing stays `owner_unavailable`. A graph route refused before launch
  (`rejected`, never launched) maps to `adapter_unavailable`, so the graph
  tries the next route.
- **Provider launch specs.** Lane P's `LaunchSpec` (#1411) is consumed behind
  `Providers::prepare -> PreparedLaunch` once it merges: confinement compiles
  the plan, wraps the sandbox and the result maps to argv, env, stdin and cwd.
  Main's adapter launch type is still uninhabited, so this branch keeps the
  seam compiling against main and the consumer lands as a follow-up.

Supervisor/confinement must accept the holder and writer-reservation protocol
before step 2; Fabric must accept event reconciliation and continuation before
step 5. These are owner acceptance conditions,
not permission to ship an unresolved custody path. The chair arranges the
cross-family council and accepts the design; the lane opens a PR and does not
merge or arm auto-merge.
