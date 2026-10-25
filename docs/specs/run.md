# Execution: runs

## Purpose and ownership

This spec owns how a request becomes bounded provider attempts: admission,
batch scheduling, retries, continuation, attempt lineage, cancellation and the
publication of terminal facts. It retains the exact prompt, route and result of
each attempt, so completion and cancellation survive a disconnected caller.
The [cross-cutting decisions](README.md#cross-cutting-decisions) bind it.

| Crate | Owns |
| --- | --- |
| `run` | Operations, admission tickets, the run index, attempts, status, event and seen cursors in `run.sqlite3`, and the run directories. |
| `records` | The shared typed records, caps and `AttemptFact`. |

Routing resolves models and effort, and providers build launches and decode
their own wire formats ([routing](routing.md)). Supervisor owns every process,
capture, observation and signal ([supervisor](supervisor.md)). Fabric owns
identity, federation placement and MCP rendering, and keeps no second run
registry ([fabric](fabric.md)). Run starts no process itself and keeps no
second route catalogue. Workflow owns delivery acceptance; run reports
`delivery_id` only as a correlation.

## Boundaries

- Admission and operation insertion are one transaction. No provider launch,
  resource probe, wait or supervisor call happens inside a transaction.
- Terminal attempts are immutable. A retry or a continued turn is a new
  attempt, and the latest attempt of a task is the highest attempt number,
  never the latest timestamp.
- A lost reader, a cancelled wait or a dead scheduler never stops provider
  work. Silence is an observation, never an outcome or a stop request.
- Run reads no provider transcript for prompt, message or usage data. The one
  allowed read is the post-exit identity projection for Codex (session ID,
  model, effort). Missing identity stays unknown.
- No old-root discovery, alias repair or record migration. Requests are strict
  and IDs unambiguous.
- Contained freedom (how a lane is confined and signalled) belongs to
  [supervisor](supervisor.md) and
  [ADR 0027](../adr/0027-rust-front-door.md); run only consumes its results.

## Contract

### Commands

The verbs are top-level: `dispatch`, `lanes`, `output`, `cancel`, `resume` and
`reconcile`. There is no `run` or supervisor control subtree. All accept
`--json`, `--state-dir` and `--project`; [cli](cli.md) owns the exact grammar
and `provenant help <verb>` lists the flags.

| Verb | Behaviour |
| --- | --- |
| `dispatch` | One prompt (`--prompt-file` or `--prompt-stdin`) or `--manifest`. `--operation-id` is required and makes retries idempotent. Optional `--task-id`, `--delivery-id`, `--host` and, for delivery, `--deliver-remote`, `--deliver-branch`, `--open-pr`, `--branch` and `--base`. Detached unless `--wait` is given. It returns exact IDs, route, state and artifact paths. |
| `lanes` | Lists lanes by `--id`, `--state`, `--since`, `--all`, `--limit` (1 to 100) and `--cursor`. With `--wait` it waits for `--condition any` (default) or `all`. Read-only otherwise; no implicit repair. |
| `output` | Reads one attempt's `--part result` (default), `prompt`, `stderr` or `events`, by `--offset` or `--tail`, up to `--max-bytes` (default 65536). It reports offsets, EOF, clipping, encoding and completeness. |
| `cancel` | Target `RUN[/TASK[/ATTEMPT]]` with a required `--operation-id`, optional `--reason` and `--wait`. It returns requested, already terminal or unobservable, plus custody. Accepted cancellation does not prove exit. |
| `resume` | A prior attempt, a required `--operation-id` and a new prompt. The turn keeps route, access and session. It creates a new attempt linked by `retry_of`; a fresh route is a new `dispatch`. |
| `reconcile` | Settles open runs (all, or `--id`, repeatable) and reports uncertain custody. `--abandon-uncertain` is refused in this build. It never replays an uncertain launch. |

Route and access inputs on `dispatch`:

- Exactly one `--model ADAPTER/ID` or `--route NAME`, an optional `--effort`,
  and an optional `--fallback` of at most two models. `--confidential` and
  `--allow-free` are the routing filters in [routing](routing.md). Each
  attempt records its resolved account label. `--account NAME` pins a
  registered account key or label for the lane, like a manifest task's
  `account`; it works with every access flag, and in a manifest it is the
  default for tasks that name none. An unknown or ineligible account fails as
  a manifest task's does.
- `--mode read-only` (default) or `worktree-write` with `--worktree`. Optional
  `--read-root` and `--write-root` (repeatable), `--network on|off` (on for
  writers, off for read-only), `--capability` (postgres) and `--knob NAME=VALUE`.
  Confinement checks the caller's authority; flags cannot grant it.
- `--provider-timeout SECONDS`: default 3600, or 10800 for `worktree-write`,
  range 1 to 14400. This is the provider budget, distinct from a reader wait.
- Manifest: strict JSON `{schema_version:1,tasks:[...]}` with 1 to 64 tasks,
  unique optional task IDs (32 lowercase hex digits; omitted IDs are
  generated), `--concurrency` 1 to 8 (default the `lanes.max_concurrency`
  knob, 8). Overlapping writer scopes reject before launch; a task-local refusal does not suppress valid siblings.

### Waits

A CLI wait takes an explicit 1 to 14400 seconds and an MCP wait 1 to 55. There
is no default, and zero is not a budget. `wait any` returns on the next unseen
terminal or `input_required` lane. `wait all` returns when every lane is
terminal or any is `input_required`, so attention never waits behind a running
sibling. The selected set is fixed at entry and excludes the caller and its
ancestors. Unreachable hosts stay unknown. Run stores seen cursors by actor and
selection; only displayed rows are acknowledged, and clipped rows stay unseen.

Exit codes follow the [shared contract](README.md#cross-cutting-decisions). A
satisfied wait exits 0, including `input_required` and unsuccessful terminal
outcomes, and reports them in `data`. A provider timeout is recorded as
`timed_out`, distinct from reader expiry (124).

### Records, layout and publication

The state root comes from [state](state.md). Paths are relative to it, and run
IDs are opaque core IDs.

```text
run.sqlite3                    # operations, admission, runs, attempts, events, cursors
runs/<RunId>/                  # ProjectId is recorded, not a parent directory
  request.json                 # immutable canonical submission and prompt digests
  attempts/<AttemptId>/
    prompt.txt                 # run-owned immutable prompt
    result.md                  # run-owned sealed result, including failure
    (supervisor files)         # custody, launch, process, exit, cancel, stdout, stderr
    provider-state/            # retained provider history; no credentials
    quarantine/                # retained patches and blobs
    scratch/                   # disposable launch staging only
    tmp/                       # attempt-private temporary data, mode 0700
```

The supervisor file names inside an attempt directory are the run and
supervisor contract; [supervisor](supervisor.md#records-and-state) owns them.
Structured events live in SQLite, and provider parsing uses the live stream, not
a second copy. Only `scratch/` is deleted at settlement; history and transcripts
stay retained, and no retention or clean command exists.

Nested submission uses a dedicated store under the submitting attempt, selected
by `PROVENANT_RUN_STATE_DIR`, while identity and config stay on the original
root. A host-side worker launches only runs whose operation is accepted. See
[nested lanes, MCP and hooks](routing.md#nested-lanes-mcp-and-hooks).

A project may grant write exceptions to a read-only run in
`projects/<ProjectId>/read-only.json` (`{"readOnlyWritable": []}` by default).
Entries name existing directories relative to the verified repository root;
absolute paths, traversal, symlinked components and Git or provider
configuration components are refused, and the attempt records every granted path.

The database is rusqlite with WAL, foreign keys and `synchronous=FULL`. Records
use `schema_version:1`, strict fields and tagged enums; unknown versions fail
closed without modification. Caps: a JSON record or a prompt 1 MiB, a captured
stream 64 MiB, a result 4 MiB, an `output` read 64 KiB. An oversized result is
a typed failure, never a certified truncation. Every terminal attempt has a
`result.md`, possibly empty, with a completeness flag and digest.

Stored records:

- `RunRecord`: run, project and actor, operation ID, request digest, task IDs,
  continuation links, optional nested `parent_attempt`, and closure state
  `open|closing|closed`.
- `AttemptRecord`: IDs, optional `delivery_id`, `retry_of`, requested, resolved,
  applied and observed route and effort, confinement, subject identity,
  deadlines, custody, timestamps, state, outcome, usage with source, artifact
  paths and digests, and publication errors.
- `AttemptFact`: a `Serialize`-only terminal projection built from validated
  records. It carries the resolved route and account label, usage, reviewer,
  `delivery_id`, `queued_at`, optional `started_at` and `provider_ms`. It is
  what telemetry and workflow read.
- `OperationRecord`: scope, kind, request digest and
  `prepared|accepted|terminal|launch_unknown`. The digest covers prompt, route,
  authority and execution controls, not reader waits.
- `AdmissionTicket`: operation, attempt and project IDs, sequence, holder
  custody, reserved counts, memory and disk floors, startup spacing and the
  admission deadline.

Host admission bounds active lanes at 64 per host and 16 per project, and
queued lanes at 256 per host. The defaults are a 5 per cent memory floor, a
500 MiB disk floor, 20 s startup spacing and a one-hour admission deadline.

Attempt states run `queued`, `starting`, `running`, `settling`, `terminal`;
refusal, queued cancellation and spawn failure can end before `running`. The
outcomes are `succeeded`, `failed`, `rejected`, `cancelled`, `timed_out`,
`rate_limited`, `quota_limited`, `auth_required`, `model_unavailable`,
`permission_denied`, `quarantined`, `lost` and `input_required`. A dead owner
is not a failed provider: unresolved custody stays an observation until
supervisor proves exit or loss. A natural completion wins over a late
cancellation or deadline. Success needs supervisor-observed exit 0, a matching
provider identity and a complete, non-empty, validated result.

Publication order is load-bearing:

1. One transaction reserves the operation and digest, IDs and admission tickets.
   The same key and digest returns the receipt; a different request is
   `operation_conflict`. Queue overflow rejects before acceptance, and the
   immutable request is published before acknowledgement.
2. The scheduler samples resources outside the transaction, rechecks capacity
   while claiming a ticket, takes writer custody and hands supervisor the
   plan. A lost acknowledgement records `launch_unknown`, and capacity and
   writer reservations stay held.
3. Result and exit evidence are sealed first. Then one commit writes the
   terminal attempt, run summary, operation state, released capacity and event.
   Capture bytes are never completion evidence.
4. Recovery retries a pending commit from sealed artifacts and supervisor facts
   without replacing a committed outcome.
5. A run closes only when every task and registered child is terminal. Closure
   neither prunes evidence nor decides delivery acceptance.

Reserve terminal-publication capacity before launch. If publication keeps
failing, the attempt stays `settling` with `publication_pending:true`, and
custody and writer reservations stay held. Run never reports success or
releases a writer early to hide a bookkeeping failure.

## External contracts

- Provider wire formats, accounts and continuation are the provider modules'
  ([routing](routing.md)); run stores what they report and never guesses a
  model or effort.
- Read-only fallback is explicit and needs a typed transient failure within the
  retry budget. A writer is never replayed automatically, and every changed
  route or effort is visible as a new attempt.
- `fabric` MCP tools render the same run API; MCP waits cap at 55 seconds.

## Failure and recovery semantics

- A scheduler that dies leaves queued work; `reconcile` recovers it, reattaches
  or checks holders, finishes pending publication and repairs summaries.
- A starting attempt with no launch summary, no held custody lock and no
  recorded provider start is settled as rejected ("launch interrupted") and its
  writer reservation released. A held lock stays unobservable.
- Refusals and conflicts are typed and exit 1 or 2; setup, I/O and contention
  exit 3. Invalid or unknown fields, negative values and range overflow fail
  validation.
- Output byte offsets count original bytes; lossy text is labelled `utf8-lossy`
  and diagnostics are redacted stderr only.

## Deliberate cuts

| Cut | Why |
| --- | --- |
| Direct `cf_dispatch`, `--chain`, `--plan-only`, `--doctor` | Replaced by routing and provider library calls. |
| Run-directory scaffolding, `RUN_RECEIPT.json`, `MANIFEST.md`, manual finaliser | One typed attempt record replaces them. |
| `inspect`, `show`, `prompt`, `result`, `diagnostic-log` verbs | Folded into `lanes` and `output --part`. |
| `reduce` and synthetic reduction prompts | Dispatch a normal read-only task with explicit result references. |
| Arbitrary detached commands and PID markers | Supervisor-owned typed completion replaces them. |
| `fallback:any`, silent effort downgrade, automatic writer replay | Routing supplies a bounded plan; changes are visible. |
| Alias coercion, typo repair, old-root scan, migration | Strict typed requests; new state starts empty. |
| `--context-ceiling` and assurance dispatch flags | Workflow owns assurance; routing owns eligibility. |
| Watchdog silence as an outcome | Only supervisor's closed stop reasons can signal a lane. |

## Verification

- Store and publication invariants (idempotent operations, immutable terminal
  attempts, `launch_unknown`, pending publication) are tested in
  `tools/front-door/crates/run/src/tests.rs` and `dispatch_tests.rs`.
- Admission and delivery are covered by `delivery_tests.rs` and the admission
  module's unit tests.
- CLI grammar, ranges and exit codes are checked in the `cli` crate tests.
- Run `provenant help <verb>` to confirm the flags above. CI runs the full
  suites; do not run them locally.
