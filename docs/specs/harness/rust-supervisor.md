# Spec: Rust lane supervisor and a single Rust front door

> Legacy source references below describe the pre-rewrite implementation.
> Use `provenant help` for current Rust commands and the accepted
> [Rust designs](../rust-port/design/README.md) for owners.

Status: **superseded** by PORTING.md version 7 and the accepted Rust area
designs. The rollout below is historical, not an operator procedure.

Original proposal: 2026-10-23, revised after a cross-family review. Builds
on `origin/main` at `59bb0ba4`, after
[#1207](https://github.com/mblauberg/provenant/pull/1207) and
[#1209](https://github.com/mblauberg/provenant/pull/1209), and preserves their
behaviour, including the disk admission floor. GitHub owns delivery state;
this file owns the durable requirements. It grants no authority beyond what
the user approves by accepting it, and it does not relax any gate in
`HARNESS.md`.

## Outcome

One Rust binary, `provenant`, is the only front door and also holds every
provider process from spawn to its single terminal record. Python keeps the
decisions: routing, prompts, adapter argv and environment, admission, how a
provider's output is read (live and after exit), receipt content and
telemetry. Rust keeps the mechanics: process group, lock lifetimes, liveness,
capture, watchdogs, cancellation, reaping and publication. Rules that today
live in timing and race tests are carried by types, ownership and a sans-IO
state machine, and those tests are deleted.

Design rule: **Python decides, Rust holds and commits.** Python decides what a
file says and what an output line means. Rust holds the processes and locks,
writes each file once, when it should, and is the only code that signals a
provider.

## Why

- The defects of the last fortnight cluster in lane supervision: reaper custody
  ([#1046](https://github.com/mblauberg/provenant/pull/1046)), signal flakes
  (#1052), leaked children (#1088), nested owner sparing (#1133), an orphan
  reaper that SIGTERMed live runs because a host field was dropped (#1173),
  cancel cascade identity (#1174, #1185), process-table waits in tests (#1147,
  #1148), and [#1204](https://github.com/mblauberg/provenant/issues/1204),
  where ENOSPC on a bookkeeping write took an exception path in the owner that
  SIGKILLed a healthy provider.
- Each fix added tests that pin a timing or an interleaving, not a rule. The
  tests this plan deletes are 7% of test-body lines but hold 44% of the
  `sleep(` calls and 70% of the signal and kill references in Python tests.
- 51 references in `tests/test_dispatch_run.py` (46 test functions, 1,459
  lines) substitute `CF_DISPATCH` with a fake adapter. That routes the owner
  through a `Popen` branch (`dispatch_run.py` around line 2886) that
  production never takes: the real `cf_dispatch.py` makes the owner call
  `provider_exec.execute` in-process. Most of those tests check security and
  receipt policy, so they move to a fake provider behind `lane run`; only the
  two that test the seam's own receipt transport are deleted.
- The Rust front door ([ADR 0027](../../adr/0027-rust-front-door.md),
  `tools/front-door`, 1.8k lines) already passes the parity suite, so a
  toolchain, a digest identity and an install path exist.

## Boundary

### Responsibilities

| Responsibility | Today | After | Slice |
|---|---|---|---|
| Spawn provider in its own session and group | `provider_exec._run_attempt` | Rust `lane run` | S2 |
| Read loop, bounded byte capture of stdout and stderr | `_run_attempt`, `BoundedCapture` | Rust | S2 |
| Live reading of output: terminal events, Claude preamble, reopened turns, retry supersession, stderr failure rules, semantic retention | `consume`, `consume_diagnostic` in `_run_attempt` | The same Python code, run as the observer coprocess | S2 |
| Startup and wall deadlines; silence observations; CPU and writer-tree progress | `_run_attempt`, `WorkspaceProgress` | Rust | S2 |
| Descendant census, sparing, staged SIGTERM and SIGKILL, subreaper | `_Descendants`, `_enable_subreaper` | Rust | S2 |
| Signalling a provider on cancel | TS `signalRecordedRun` (`process-identity.ts`) signals owner and provider; Python owner | Only `lane run`; TS signals the owner group alone | S2 |
| Process exit record | Adapter receipt fields `pgid`, `evidence.exit`, `reaped`, `spared` | `supervisor.exit.json`, mapped by Python | S2 |
| Run custody, worktree and admission lease lifetimes; owner record; publication; `dispatch-status.json` close | `dispatch_run.py`, `run_lineage.py`, TS `execution.ts:513-527` | Rust `lane own`; Python steps render the bytes | S3 |
| Retry waits, cancel-marker watch, nested child cancellation | `dispatch_run.py`, `run_lineage.cancel_children` | Rust `lane own` (asks children, never signals them) | S3 |
| Liveness and orphan reaping | TS process identity (`process-identity.ts`), `dispatch_run.reap_attempts` | Rust `lane status`, `lane reap` | S4 |
| Routing, catalogue, quota, cooldown, memory and disk admission decisions | `cf_dispatch.py --plan-only`, `model_route.py`, `memory_admission.py` | Python, unchanged | none |
| Prompt building, adapter argv and environment, confinement, agy home staging | `provider_exec.build_plan` | Python, unchanged | none |
| Output parsing, classification, quarantine, receipt and status content | `parse_output`, `classify_failure`, `terminal_contract`, `close_mcp_run`, `reap_stop_status` | Python | none |
| Batch scheduling, batch custody proof, batch signals to its own children | `batch_run.py`, `run_lineage` | Python, unchanged | none |
| Marker cancel (`run cancel`), explicit-PID `reap`, telemetry, Fabric store, MCP, graph runner | Python and TS | Unchanged | none |

### Process topology

```text
today  fabric -> dispatch_run.py owner -> provider (own session)
S2     fabric -> dispatch_run.py owner -> provenant lane run -> provider (own session)
                                                       \-> provider_exec.py observe
S3     fabric -> provenant lane own -> provenant lane run -> provider (own session)
                     |                                 \-> provider_exec.py observe
                     \-> dispatch_run.py plan | settle | close (short-lived)
```

`lane run` is always a separate process in its owner's process group. A group
signal reaches it and becomes an ordinary interrupt; an owner that receives a
signal alone forwards it to its own unreaped `lane run` child. If the owner is
SIGKILLed, `lane run` keeps supervising, writes the exit record, and the
reaper settles the attempt from it (see Lifetimes). Nothing is resident
between attempts, so the provider-execution-core non-goal "no scheduler,
daemon or second lifecycle engine" still holds.

### Interface: `provenant lane run` (S2)

- **In:** stdin. The first line is the launch spec, `provenant.lane.launch.v1`.
  The environment carries credentials, so the spec never touches disk;
  `launch.json` records it with environment values replaced by their names.
- **Out:** stdout, one JSON event per line, best effort: `started` (pid, pgid,
  canonical start time), `progress` (at most once a second), `exited`. The S2
  Python owner calls `AdmissionLease.started()` on `started`. A broken pipe is
  ignored.
- **Files:** `events.jsonl`, `stderr.log` and `pgid` at today's paths and
  formats; `supervisor.lock` (held for the attempt's life); `supervisor.json`
  (running state, replaced atomically); `supervisor.exit.json` (the terminal
  process record, created once with no-replace semantics); `launch.json`.
- **Exit status:** 0 when the exit record is published, whatever the provider
  did; 64 for an invalid spec (nothing started); 75 when the record could not
  be published.

```json
{"schema": "provenant.lane.launch.v1",
 "argv": ["/usr/bin/sandbox-exec", "-f", "...", "codex", "exec", "..."],
 "cwd": "/abs/worktree", "env": {"PATH": "...", "OPENAI_API_KEY": "..."},
 "stdin": {"path": "/abs/run/tasks/t/attempt-001/prompt.md"},
 "attempt_dir": "/abs/run/tasks/t/attempt-001", "run_token": "...",
 "capture": {"events": "events.jsonl", "stderr": "stderr.log", "max_bytes": 8388608},
 "silence_observation_ms": 300000,
 "timeouts_ms": {"startup": 300000, "wall": 3600000,
                 "settle": 1500, "child_grace": 500, "root_flush": 2000},
 "progress": {"writer_tree": {"root": "/abs/worktree", "exclude": [".git", "node_modules"]}},
 "cancel_markers": ["cancel.request", "../../batches/b1/cancel.request"],
 "observer": {"argv": ["/abs/python3", "/abs/provider_exec.py", "observe", "--adapter", "claude", "..."]},
 "spare": {"code_mode_host": true}}
```

**Silence.** `silence_observation_ms` comes from `lanes.silence_seconds`.
After that interval without stdout or stderr, Rust samples liveness, output
timestamps and writer-tree progress and writes `watchdog.json` beside the
capture files. Silence never reaches `ProviderGroup::stop`; it cannot become
a forced reason, even when the writer tree is unchanged. Startup and wall
deadlines remain separate stop reasons. The retired `stalled` forced reason
is rejected, including in observer requests. After proven process death, Python
builds `wip-checkpoint.json` from retained events and a bounded Git summary;
Rust commits those bytes for the owner from S3 and the reaper from S4.
Checkpoint failures add a warning and never
block terminal publication. The reaper skips Git for read-only lanes and
shares a one-second Git budget across all dead writers in a pass.

**Observer.** Live reading stays in Python, in the code that does it today.
`lane run` starts the observer and writes it every captured line as
`{"s": "out" | "err", "line": "..."}`. The observer runs the existing
`consume` and `consume_diagnostic` logic, including the Claude preamble rule,
reopened turns, retry-error supersession and the kiro and opencode stderr
rules, and replies with at most two kinds of request: `{"terminal": true}`
(start terminal grace and close the provider's stdin) and
`{"stop": "<reason>"}` (staged stop, recorded as `forced`). On stdin EOF it
writes `observer.json` (session id, observed model, reset and retry hints,
live failure, retained semantic lines) and exits; `settle` reads it. There is
no matcher language. `lane run` never blocks on the observer: lines go
through a bounded queue, and a full queue drops lines with a count. A dropped
line or a dead observer leaves the watchdogs in charge and is recorded as
`observer_lost`, and `settle` falls back to parsing the capture files. The
observer protocol is frozen only after each adapter's recorded transcripts
replay through it with the same live decisions `_run_attempt` makes today;
until then the knob is per adapter.

**Numbers.** Durations are integer milliseconds. The shared `json.rs` reader
keeps no value for a fractional number (`Value::Float`, `json.rs:11-12`), so it
gains a value-carrying float before the lane uses it, and Rust rejects a
fractional field it needs. Rust never re-serialises a document Python wrote.

`supervisor.exit.json` (`provenant.lane.exit.v1`) holds `pid`, `pgid`,
`started_at`, `ended_at`, `last_progress_at`, `exit`, `signal`, `forced`
(`null`, `cancelled`, `startup_timeout`, `wall_timeout`,
`supervisor_signal`, or an observer `stop` reason), `terminal_event_seen`,
`reaped`, `spared`, `census_warnings`, capture byte counts with `truncated`,
`observer_lost` and `io_errors`. It is bounded at 16 KiB: lists are truncated
with a count. Python maps it onto today's adapter receipt fields, so
`attempt.json` does not change.

### Interface: `provenant lane own` (S3)

Fabric's `startOwner` execs `provenant lane own` with today's dispatch
arguments. The owner takes run custody before writing anything, then loops:

1. `dispatch_run.py plan` (short-lived) takes the worktree lease and memory
   and disk admission on descriptions the owner passes it (see Lifetimes) and
   returns `{"launch": <spec>, "active": <path of staged attempt.json>,
   "admission_settle_ms": 20000}` or `{"terminal": <staged bundle>}` for a
   refusal. Cancelling `plan` is safe because no provider exists yet.
2. The owner commits the active record, starts `lane run`, and releases the
   admission lock `admission_settle_ms` after `started`.
3. On `lane run`'s exit, the owner takes the attempt and children-journal
   locks and runs `dispatch_run.py settle` under them. `settle` parses output,
   quarantines instructions, reads registered children and stages the
   terminal bundle; it returns `{"bundle": [...], "next": "done" |
   {"retry_after_ms": n} | "fallback"}`. The owner commits the bundle before
   releasing the locks, then sleeps any retry delay while watching the cancel
   markers.
4. On `done` it commits the run bundle `dispatch_run.py close` stages
   (`RUN_RECEIPT.json`, `dispatch-status.json`), removes
   `dispatch-owner.json`, and exits.

Nested children are cancelled by writing their `cancel.request` and waiting a
bounded time for their terminal record. The parent never signals a child's
processes, so it never has to prove a child's identity from its environment.

### Lifetimes

`flock` belongs to an open file description, so a lock taken through a
description that two processes share stays held until both close it. The
owner opens each lock file, hands the description to the Python step with an
environment variable naming the descriptor (the inherited-descriptor pattern
of [#1209](https://github.com/mblauberg/provenant/pull/1209)), and the step's
existing code takes or proves the lock and exits without unlocking. The
admission wait still unlocks between its 15-second polls, because `LOCK_UN`
acts on the shared description. Every other descriptor is `O_CLOEXEC`, and
none reaches the provider or the observer.

| Lock | Taken by | Held by, until |
|---|---|---|
| Run custody (`MANIFEST.md`) | `lane own`, first act | `lane own`, exit |
| Batch custody (the batch's `MANIFEST.md` description) | `batch_run.py` | `batch_run.py`, batch end. A batch child proves it through the inherited descriptor and closes it, as today (`batch_run.py:674-677`, `run_lineage._check_borrowed_custody`); it never reacquires |
| Worktree writer lease (`<git-dir>/` writer lock) | `plan`, on the owner's description | `lane own` until run end, and `lane run` until its provider group is gone |
| Memory admission (`$XDG_STATE_HOME/provenant/admission.lock`) | `plan`, on the owner's description | `lane own`, 20 s after spawn or attempt end (`memory_admission.py:36-58`) |
| Attempt publication and children journal | `lane own` | `lane own`, through `settle` and the bundle commit |
| `supervisor.lock` | `lane run` | `lane run`, until the exit record is written |

In S2 the Python owner keeps today's leases unchanged; only spawn moves.

`lane run` holding a duplicate of the writer lease means a SIGKILLed owner
cannot free the worktree for a second writer while its provider still edits.
After such a kill the reaper waits while `supervisor.lock` is held, then runs
`dispatch_run.py settle --reaped` on `supervisor.exit.json`, so the attempt
records its observed exit. Without an exit record it falls back to today's
synthesis from absence (`provider_lost`, `reap_attempts`).

### Publication

- **Opaque bytes.** Python steps stage files in an owner-created `0700`
  directory beside the attempt. Rust moves them into place unchanged and
  reads only `state`, `status` and `next`. Digests (the embedded `digest` of
  the canonical record, the `attempt.sha256` sidecar of the legacy envelope)
  are computed by Python over those bytes; Rust recomputes each sidecar before
  the commit and refuses a mismatch.
- **Two attempt records.** The canonical `tasks/<task>/attempt-NNN/attempt.json`
  (`fabric.attempt.v1`, with `state`) and the legacy
  `dispatch/tasks/<task>/attempt-NNN/attempt.json` (`record_type:
  dispatch-attempt`, `finished_at`) are both in the bundle.
  `receiptTerminal` in `process-identity.ts` reads the canonical record first and falls back to
  the legacy one only when the canonical is absent. The active canonical
  record exists from launch, so a reader never falls back mid-bundle.
- **Bundle order.** Adapter receipt, `result.md`, legacy envelope and its
  sidecar, manifest row, then the canonical terminal `attempt.json`, created
  with `link(2)` so it cannot replace a terminal record. A reader that sees a
  terminal canonical record sees a complete bundle. Children registered while
  `settle` ran waited on the journal lock and see terminal state, as
  `publish_contract` guarantees today.
- **`dispatch-status.json`.** TS writes the initial record before spawning the
  owner (`execution.ts:421-444`); no other writer exists yet, so that stays.
  From S3 TS stops its completion merge (`execution.ts:513-527`): `close`
  folds in `termination-request.json`, so a TS-requested `cancelled` is
  preserved, and TS only reads the file.
- **Full disk.** #1207's floor (500 MiB free by default, `disk_floor_bytes`)
  starts each attempt with headroom. At spawn `lane run` writes a 64 KiB
  `supervisor.reserve`; on ENOSPC at publication it unlinks it and retries
  once. That is a retry, not a guarantee: APFS snapshots or a competing
  writer can keep the space. If the retry fails, `lane run` exits 75 with
  the record on stdout, and the reaper closes the attempt as `host_disk_full`
  (#1207). Either way the provider is never signalled over an I/O error.

### On-disk formats

| File | Writer today | Writer after | Change |
|---|---|---|---|
| `events.jsonl`, `stderr.log`, `pgid` | Python | Rust (S2) | None |
| `watchdog.json` | Python supervisor or reaper | Rust supervisor (S2); Python reaper until S4 | Silence observation only; never a stop request |
| `wip-checkpoint.json` | Python owner or reaper after proven death | Python content; Rust commit for owner (S3), reaper (S4) | Best effort before terminal publication |
| `supervisor.lock`, `supervisor.json`, `supervisor.exit.json`, `launch.json`, `observer.json` | none | Rust and the observer (S2) | New, versioned |
| Both `attempt.json` records, sidecars, adapter receipt, `result.md`, manifest, `RUN_RECEIPT.json` | Python | Python bytes, Rust commit (S3) | None |
| `dispatch-owner.json` | Python | Rust (S3) | Same fields |
| `dispatch-status.json` | TS and Python | TS initial record, Rust close (S3) | Same fields |
| `cancel.request`, `termination-request.json` | Python, TS | Unchanged | None |

A launch spec with an unknown major version is refused with exit 64 and
`fix: install a compatible Provenant release`. The owner does not fall back to
Python.

## Structural guarantees

| Guarantee | Mechanism | Bug class it removes |
|---|---|---|
| Bookkeeping cannot reach a kill | Only `ProviderGroup::stop(StopReason)` signals. `StopReason` is a closed enum (cancel, startup or wall deadline, observer `stop`, supervisor signal, post-exit cleanup); silence and I/O errors have no conversion into it, and the writer module never receives the group handle. From S2 no Python process holds a provider | #1204: ENOSPC in `publish_contract`, then `finally: descendants.stop()` |
| PID and PGID cannot be reused under a signal | `lane run` is the provider's parent. It observes exit with `waitid(P_PID, pid, WEXITED \| WNOHANG \| WNOWAIT)`, never `Child::try_wait`, which reaps. Group cleanup runs while the zombie leader still reserves its PID and therefore its group ID; reaping, by consuming the handle, is the last act | PID reuse in reaping and cancellation; start-time string matching |
| No missed fast exit | `SIGCHLD` is registered with the event loop before spawn. `EVFILT_PROC` is registered after spawn and fails with `ESRCH` if the child has already exited; both paths end at the same `WNOWAIT` check | Exit lost between spawn and watch |
| Signal dispositions are known | At start `lane run` resets `SIGCHLD` to default (an inherited `SIG_IGN` makes the kernel reap the leader and free its group ID) and ignores `SIGPIPE`; `SIGTERM`, `SIGINT` and `SIGHUP` arrive as loop events. The provider gets an empty mask and default dispositions before exec | Inherited ignored signals; handler re-entrancy |
| Liveness without identity inference | An attempt is live while `supervisor.lock` is held, a run's owner while run custody is held; both are released by the kernel on exit. A non-blocking probe decides, and records are read only to find what to clean up | #1173; host-liveness probes and permission-denied ambiguity |
| Terminal records written once | `Lane<Exited>::publish(self)` consumes the state and hands it back on error. Terminal files are created by `link(2)` from a complete temporary file, which fails if the name exists, so a supervisor and a reaper racing to close an attempt cannot both win | Reaper versus late publisher |
| Leases last as long as the provider | `lane run` holds the writer-lease description until the provider group is gone | Second writer after an owner crash |
| Races are ordered events | Signals, child exit, pipe readiness, observer requests, cancel markers and deadlines arrive in one loop (kqueue on macOS; signalfd, pidfd and poll on Linux). A pure `step(state, event) -> (state, actions)` decides | Signal during spawn return, late signal after exit, marker versus natural exit |
| One cleanup path | `Drop` for `ProviderGroup` runs the staged stop if the supervisor unwinds; the release profile moves from `panic = "abort"` to unwinding | Cleanup skipped or doubled on exceptions |
| Nested owners spared by kernel fact | A descendant is spared when it holds the run custody or `supervisor.lock` of a run registered in this attempt's children journal, and that run's `dispatch-owner.json` names its PID | Forged token, sibling environment, owner replaced after enumeration (#1133, #1174, #1185) |

What Rust does not fix: `EVFILT_PROC` observes the leader's exit, not its
descendants, and macOS has no process containment. A descendant that calls
`setsid` and is reparented before the first census is still unattributable.
The census rules are ported with typed `(pid, start_time)` identities. Linux
cgroup containment is possible later but only CI runs Linux, so it is out of
scope.

## Test strategy

### Lane conformance suite

A black-box pytest suite under `tests/lane_conformance/`, run against the
Python owner and then unchanged with `PROVENANT_LANE_IMPL=rust`. It drives a
real dispatch and asserts only on retained files and the process table.
Its fake providers are synchronised: they block on a FIFO the test controls,
so no scenario depends on a sleep.

S0 seeds it with today's black-box invariants (one attempt per ordinary
dispatch, exit status, startup timeout, SIGTERM to the owner, cancel during a
run, nested owner spared). Each later slice adds the rows it needs before it
switches its default:

| Slice | Rows added |
|---|---|
| S2 | Silence observations with and without writer-tree progress, without signalling; per-adapter wall deadlines; checkpoint write failure preserves terminal publication; cancel before launch, during run and after natural exit; provider ignoring SIGTERM; new-session grandchild; terminal event with stdin open; capture over the cap; fast exit before watch; ENOSPC on capture, progress and the exit record (fault-injection build); observer death; per-adapter transcript replay through the observer |
| S3 | Owner SIGKILLed mid-attempt (provider finishes, reaper settles from the exit record, second writer refused meanwhile); two reapers and a late supervisor; child registering during publication; admission lock held through spawn settle; batch child under inherited custody |
| S4 | `lane status` on live, finished and SIGKILLed runs; `lane cancel` through TS; orphan reap with a live host |

ENOSPC uses a `fault-injection` Cargo feature, built only for the suite. The
suite runs in CI on Linux and on a new macOS arm64 job for changes under
`tools/` and the lane scripts (public repositories get macOS runners free);
ADR 0027's "no macOS CI leg" is amended for this crate. A nightly stress job
runs the suite 50 times on both platforms and files an issue on any failure;
it is not a merge gate. Rust unit tests cover `step()` as transition tables
and run without processes. The suite is never deleted: once Python goes, it
is the lane's contract tests.

### Deletable tests

The exact inventory, test by test, is in
[rust-supervisor-tests.md](rust-supervisor-tests.md). Counts are test-body
spans on `origin/main` at `59bb0ba4`.

| Slice | Firm | Conditional | What goes |
|---|---|---|---|
| S1 | 1,549 (123 tests) | 0 | The parity suite except `test_stale.py`; Python dispatcher and template tests |
| S2 | 1,826 (70) | 75 (4) | Census, sparing, reaping, signals, watchdogs, writer progress, byte capture, ENOSPC during capture and progress, subreaper; the two fake-transport receipt tests |
| S3 | 830 (23) | 98 (3) | Cancel cascade identity, owner records, publication custody, lease release, late signals |
| S4 | 832 (36) | 758 (35) | Reaper identity races, TS process identity and orphan reaping |
| **Total** | **5,037 (252)** | **931 (42)** | |

That is 7.2% of the 69,537 test-body lines (5.7% of the 88,941 test-file
lines), or 8.6% with the conditional tests. It is not larger because most of
the test weight checks policy that stays in Python: confinement, routing,
quota, receipts, landing and hosts. The cut is the slow and flaky part, not
the bulk. Retained and not counted: `close_mcp_run` rendering,
`reap_stop_status` mapping, batch signal and scheduling tests, the
behind-origin warning (`test_stale.py`, re-pointed at the binary), observer
semantics (Claude preamble, opencode and kiro hangs, retry supersession), the
batch custody proofs from #1209, and the 44 `CF_DISPATCH` policy tests,
which move to the fake provider. The suite and the Rust unit tests add about
1,400 lines.

Product code removed, approximately: the Python front door and template (713
lines); the process half of `provider_exec.py` (about 1,100) and the
test-seam `Popen` branch (about 150); owner custody, cancel identity and
reaper identity code in `dispatch_run.py` and `run_lineage.py` (about 700);
the process-identity code in `process-identity.ts` (about 600). Rust added:
about 2,500 lines plus tests.

## Front door

The front door is Rust. `tools/front-door/rust-toolchain.toml` pins the build
with rustfmt and Clippy; build with `cargo build --release --locked` from that
workspace. Release identity comes from tracked source and asset inventories.
Runtime startup never builds the binary.

Installed binaries resolve `<release>/bin/provenant` beside `release.json`;
uninstalled builds require explicit `PROVENANT_ROOT`. `check` selects the
caller's Git toplevel, validates its Provenant markers and runs that checkout's
`scripts/check-harness`. The old Python checkout resolver, stale-check advisory
and build/installation helpers are retired. Legacy installer entrypoints exit
3. Production install command composition belongs to release cut-over; the
config crate currently supplies the typed release and managed-file APIs.

S1a (#1231) completed the front-door switch. S2 (#1379) removes the legacy
`lane` route and exposes `internal supervise` for the Rust supervisor. The
historical slice plan below is superseded by the
[current supervisor design](../rust-port/design/supervisor.md).

## Slices

Each slice is one or more pull requests, shippable alone. A knob selects the
Rust implementation while conformance is established. Once its conformance
rows are green, the slice switches its default and deletes its Python path,
knob and superseded tests in the same cutover. There is no timed trial or
post-trial deletion window. Rollback is a git revert; restore Python from git
history if needed. A lane-day is one implementation lane's working day.

| # | Slice | Knob before cutover | Done when | Kill criteria |
|---|---|---|---|---|
| S0 | Conformance seed: today's black-box invariants and synchronised fake providers, after #1207 merges | none | Green on Linux and macOS CI | Fake providers not deterministic within 2 lane-days: stop the supervisor work (S1 continues) |
| S1 | Rust as the only front door | none after cutover | S1 conformance green; both Macs on the binary; Python front door and S1 tests deleted | A regression requiring Python: revert the slice and restore it from history |
| S2 | `lane run` and the observer replace the process half of `_run_attempt`, per adapter; TS stops signalling providers | `PROVENANT_LANE_IMPL=python\|rust` | S2 rows and transcript replay green for every adapter on both platforms; Rust default; S2 Python path, knob and superseded tests deleted | Parity not reached in 8 lane-days, a healthy provider killed or an exit record lost, or more than two Rust-attributable supervision fixes: revert S2, stop S3 and S4 |
| S3 | `lane own` replaces the `dispatch_run.py` owner; Python split into `plan`, `settle`, `close` | `PROVENANT_LANE_OWNER=python\|rust` | S3 rows green; Rust default; S3 Python owner path, knob and superseded tests deleted | Parity not reached in 10 lane-days, or the Python split needs more than about 600 changed lines of glue, or S2's thresholds are breached: stop at S2, which already removes the #1204 class |
| S4 | `lane status` and `lane reap`; `statusRows` in `run-status.ts` and `reapAttemptRuns` in `process-identity.ts` | `PROVENANT_LANE_CONTROL=python\|rust` | S4 rows green; Rust default; Fabric TS holds no process-identity code; graph runner unchanged at its call sites; superseded control paths, knob and S4 tests deleted | Not done in 5 lane-days, or any TS path still needs `processMatches`: keep TS control and stop |

S0 and S1 can run in parallel; S2 starts after S0. S3 starts automatically
when S2 is done with no kill criterion hit and its deletions merged. S4 starts
automatically when S3 is done. No slice retains an installable Python fallback
after cutover.

## Other areas assessed

| Area | Rust later? | Why |
|---|---|---|
| Fabric TS runtime (11k lines) | No | The MCP SDK and the SQLite store fit TypeScript; after S4 its process code is a call to the binary |
| `model_route.py` (2.7k), `model_route_pools.py`, `provider_quota.py` | No | Policy and catalogue data that change weekly; the tests are data tables, not races |
| `telemetry.py` (1.8k) | No | Batch ingest with no process ownership or latency budget |
| `provider_exec.build_plan` and the confinement profile | No | Security-relevant string building, reviewed as Python; Rust adds nothing structural |
| `fabric_git.py`, `fabric_hosts.py`, `bounded_process.py` | No | Short helper subprocesses whose defects are protocol, not supervision; a Rust hop per call costs more than it saves |
| `scripts/reap.py` (explicit-PID reap) | No | It stops an arbitrary process the caller names, so it has no parent relationship for Rust to exploit |
| `check` checkout selection | Rust | Implemented by the front door; runs the caller checkout's checker |
| `lanes --wait` and `events --follow` | Maybe, after S4 | A lock-probe wait is simple in Rust; only if wait defects recur |

## Risks

- **macOS has no process containment.** Census attribution stays a heuristic
  for processes that escape before the first census.
- **Shared lock descriptions.** A Python step that unlocks on its success
  path would release a lock Rust still means to hold. The hand-off paths
  never call `LOCK_UN`, and the S3 rows check that a second writer and a
  second admission are refused at the right moments.
- **Observer coupling.** Every captured line crosses a pipe to Python. The
  bounded queue keeps a slow observer from stalling supervision, at the cost
  of falling back to after-exit parsing.
- **The test seam hides coverage.** The 44 `CF_DISPATCH` policy tests must
  move to the fake provider in S2; some may turn out to test only the dead
  branch.
- **Two implementations before conformance.** The cutover deletes the Python
  path as soon as its conformance rows are green; rollback is a git revert.
- **Toolchain drift.** A release retains its packaged source digest after
  checkout updates. Install and install-check command composition belongs to
  release cut-over; runtime startup never rebuilds an installed binary.

## Decisions

Decided by the user: rustup with the pinned toolchain on each Mac and local
builds; S3 and S4 proceed automatically once the preceding slice's criteria
pass. On 2026-10-23 the user dropped the 7-day trials for every slice: switch
the default and delete the Python path once conformance is green; rollback
is a git revert.

Workers outside Fabric: the [direct-CLI
fallback](../../../skills/orchestrate/references/direct-cli-fallback.md) still
launches `codex exec` workers that `worker_liveness.py` reports on, so it and
its 397 lines of tests stay until that fallback retires.

## Related

- [Deletable test inventory](rust-supervisor-tests.md).
- [ADR 0027](../../adr/0027-rust-front-door.md): Rust front door; amended here
  for the `libc` dependency and a macOS CI job.
- [Provider execution core](provider-execution-core.md): the non-goals this
  keeps.
- [Graph runner](graph-runner.md): consumes run-registry liveness, whose call
  sites S4 preserves.
- [Fabric v2](../fabric-v2.md): attempt states, statuses and the cleanup rules
  ported in S2.
- [Worker liveness](../../../skills/orchestrate/references/worker-liveness.md):
  kept for the direct-CLI fallback.
- [#1204](https://github.com/mblauberg/provenant/issues/1204),
  [#1207](https://github.com/mblauberg/provenant/pull/1207) and
  [#1209](https://github.com/mblauberg/provenant/pull/1209).
