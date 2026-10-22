# Spec: Rust lane supervisor and a single Rust front door

Status: **proposed**, 2026-10-23. Lands after
[#1207](https://github.com/mblauberg/provenant/pull/1207) and preserves its
behaviour. GitHub owns delivery state; this file owns the durable
requirements. It grants no authority beyond what the user approves by
accepting it, and it does not relax any gate in `HARNESS.md`.

## Outcome

One Rust binary, `provenant`, is the only front door and also owns every
provider process from spawn to its single terminal record. Python keeps the
decisions (routing, prompts, adapter argv and environment, output parsing,
receipt content, telemetry); Rust keeps the mechanics (process group, custody,
liveness, capture, cancellation, reaping, publication). The rules that today
live in thousands of lines of timing and race tests are carried by types,
ownership and a sans-IO state machine instead, and those tests are deleted.

Design rule: **Python renders, Rust commits.** Python decides what a file
says; Rust decides when it is written, writes it once, and is the only code
that can signal a provider.

## Why

- The defects of the last fortnight cluster in lane supervision: reaper custody
  ([#1046](https://github.com/mblauberg/provenant/pull/1046)), signal flakes
  (#1052), leaked children (#1088), nested owner sparing (#1133), an orphan
  reaper that SIGTERMed live runs because a host field was dropped (#1173),
  cancel cascade identity (#1174, #1185), process-table waits in tests (#1147,
  #1148), and [#1204](https://github.com/mblauberg/provenant/issues/1204),
  where ENOSPC on a bookkeeping write took an exception path in the owner that
  SIGKILLed a healthy provider.
- Each fix added tests that pin a timing or an interleaving, not a rule.
  `tests/test_provider_exec.py` (8,956 lines) and `tests/test_dispatch_run.py`
  (6,560) are the two largest test files; Python tests total 76,506 lines.
- 51 references in `tests/test_dispatch_run.py` substitute `CF_DISPATCH` with a
  fake adapter. That routes the owner through its `Popen` branch
  (`dispatch_run.py` around line 2609), which production never takes: when
  `CF_DISPATCH` is the real `cf_dispatch.py` the owner calls
  `provider_exec.execute` in-process. Much of the signal coverage therefore
  tests a test seam.
- The Rust front door ([ADR 0027](../../adr/0027-rust-front-door.md),
  `tools/front-door`, 1.8k lines) already passes the parity suite, so a
  toolchain, a digest identity and an install path exist.

## Boundary

### Responsibilities

| Responsibility | Today | After | Slice |
|---|---|---|---|
| Spawn provider in its own session and group | `provider_exec._run_attempt` (`Popen`, `start_new_session`) | Rust `lane run` | S2 |
| Read loop, bounded capture of stdout and stderr | `_run_attempt`, `BoundedCapture` | Rust | S2 |
| Startup, idle and wall watchdogs; CPU and writer-tree progress probes | `_run_attempt`, `WorkspaceProgress`, `_cpu_stamp` | Rust | S2 |
| Descendant census, sparing, staged SIGTERM and SIGKILL, subreaper | `_Descendants`, `_enable_subreaper` | Rust | S2 |
| Signals received by the supervisor | Python handlers in `_run_attempt` and `dispatch_run` | Rust event loop | S2 |
| Process exit record | Adapter receipt fields `pgid`, `evidence.exit`, `reaped`, `spared` | Rust `supervisor.exit.json`, mapped by Python | S2 |
| Run custody lock, `dispatch-owner.json`, active and terminal `attempt.json` publication, manifest append, `dispatch-status.json` close | `dispatch_run.py` (`publish_contract`, `close_mcp_run`, `reap_attempts`) | Rust `lane own`; Python renders the bytes | S3 |
| Retry and fallback waits, cancel-marker watch, nested child cancellation | `dispatch_run.py`, `run_lineage.cancel_children` | Rust `lane own` (asks, never signals, children) | S3 |
| Liveness, external cancel and stop, orphan reaping | TS `run-registry.ts`, `run_controls.py` cancel, `scripts/reap.py`, `worker_liveness.py` | Rust `lane status`, `lane cancel`, `lane reap` | S4 |
| Routing, catalogue, quota, cooldown, memory and disk admission | `cf_dispatch.py --plan-only`, `model_route.py`, `memory_admission.py` | Python, unchanged | none |
| Prompt building, adapter argv and environment, confinement profile, agy home staging | `provider_exec.build_plan` and helpers | Python, unchanged | none |
| Output parsing, classification, instruction quarantine, receipt content | `parse_output`, `classify_failure`, `terminal_contract` | Python, run after exit | none |
| Telemetry ingest, Fabric store, MCP surface, graph runner | Python and TS | Unchanged | none |

Batch scheduling (`batch_run.py`) stays Python. Its children become Rust
owners in S3, and its cancel path calls `lane cancel` in S4.

### Process topology

```text
today   fabric (node) -> dispatch_run.py owner -> provider (own session)
S2      fabric (node) -> dispatch_run.py owner -> provenant lane run -> provider (own session)
S3      fabric (node) -> provenant lane own  -> provider (own session)
                              \-> short-lived: dispatch_run.py plan | settle
```

`lane run` stays in its owner's process group, so a group signal to the owner
reaches it and becomes an ordinary interrupt. The provider keeps its own
session, as today. Nothing is resident between attempts: a supervisor lives
exactly as long as its attempt, so the provider-execution-core non-goal "no
scheduler, daemon or second lifecycle engine" still holds.

### Interface: `provenant lane run` (S2)

- **In:** one JSON document, `provenant.lane.launch.v1`, on the supervisor's
  stdin, read to EOF. The environment carries credentials, so the spec never
  touches disk. The supervisor writes `launch.json` with environment values
  replaced by their names, for audit.
- **Out, stdout:** one JSON line per event, best-effort: `started` (pid, pgid,
  canonical start time), `progress` (at most once a second), `exited`. A broken
  pipe is ignored. Stdout is a convenience for a live Python owner, never the
  record.
- **Out, files:** the existing capture files at the paths the spec names
  (`events.jsonl`, `stderr.log`, unchanged format); `retained.jsonl` (lines the
  spec asks to keep, capped); `supervisor.json` (running state and progress,
  replaced atomically); `supervisor.exit.json` (the terminal process record,
  created once with no-replace semantics).
- **Exit status:** 0 when the terminal record is published, whatever the
  provider did; 64 for an invalid spec (no process started); 75 when the record
  could not be published after the reserve was released.

```json
{"schema": "provenant.lane.launch.v1",
 "argv": ["/usr/bin/sandbox-exec", "-f", "...", "codex", "exec", "..."],
 "cwd": "/abs/worktree", "env": {"PATH": "...", "OPENAI_API_KEY": "..."},
 "stdin": {"path": "/abs/run/tasks/t/attempt-001/prompt.md"},
 "attempt_dir": "/abs/run/tasks/t/attempt-001", "run_token": "...",
 "capture": {"events": "events.jsonl", "stderr": "stderr.log", "max_bytes": 8388608},
 "timeouts": {"startup_s": 300, "idle_s": 900, "wall_s": 3600,
              "settle_s": 1.5, "child_grace_s": 0.5, "root_flush_s": 2.0},
 "progress": {"writer_tree": {"root": "/abs/worktree", "exclude": [".git", "node_modules"]}},
 "cancel_markers": ["cancel.request", "../../batches/b1/cancel.request"],
 "stream": {"terminal": [{"all": [{"field": "type", "equals": "result"}]}],
            "retain": [{"field": "type", "in": ["system", "result"]}],
            "stop":   [{"all": [{"field": "type", "equals": "error"},
                                {"field": "error.code", "equals": "usage_limit"}],
                        "reason": "usage_limited"}]},
 "spare": {"code_mode_host": true}}
```

`stream` is how adapter knowledge stays in Python. Each adapter profile emits
declarative matchers over JSON lines: `terminal` starts terminal grace and
closes the provider's stdin (the Cursor and Claude rules), `retain` keeps early
semantic lines that a bounded capture would roll away, and `stop` ends the
attempt with a named reason. An adapter whose live rules cannot be expressed
this way stays on the Python supervisor (the knob is per adapter) until they
can.

`supervisor.exit.json` (`provenant.lane.exit.v1`) holds `pid`, `pgid`,
`started_at`, `ended_at`, `last_progress_at`, `exit`, `signal`, `forced`
(`null`, `cancelled`, `startup_timeout`, `stalled`, `wall_timeout`,
`supervisor_signal`, or a `stop` reason), `terminal_event_seen`, `reaped`,
`spared`, `census_warnings`, capture byte counts with `truncated`, and
`io_errors`. Python maps it onto the existing adapter receipt fields, so
`attempt.json` is unchanged.

### Interface: `provenant lane own` (S3)

Fabric's `startOwner` execs `provenant lane own` with today's dispatch
arguments. The owner takes the run's custody lock before writing anything, then
loops:

1. `dispatch_run.py plan` (short-lived) returns either `{"launch": <spec>,
   "active": <attempt.json bytes>}` or `{"terminal": <bytes>}` for a refusal.
   Memory and disk admission wait inside `plan`; the owner may SIGTERM that
   child on cancel, which is safe because no provider exists yet.
2. The owner publishes the active bytes, runs the S2 supervisor in-process,
   and on exit calls `dispatch_run.py settle`, which parses output, quarantines
   instructions and returns `{"attempt": <bytes>, "manifest_row": <bytes>,
   "next": "done" | {"retry_after_s": n} | "fallback"}`.
3. The owner commits those bytes and their digest sidecars under the lock,
   with `attempt.json` last, so a reader that sees a terminal `attempt.json`
   sees a complete bundle. It sleeps any retry delay while watching the cancel
   markers.
4. On `done` it closes `RUN_RECEIPT.json` and `dispatch-status.json` from bytes
   `dispatch_run.py close` returns, removes `dispatch-owner.json`, and exits.

Because Python produces the bytes, every compatibility file stays
byte-identical to what the Python owner writes today. Rust parses only the
fields it acts on (`state`, `status`, `next`).

Nested children are cancelled by writing their `cancel.request` and waiting a
bounded time for their terminal record. The parent never signals a child's
processes, so it never has to prove a child's identity from its environment.

### On-disk formats

| File | Writer today | Writer after | Change |
|---|---|---|---|
| `events.jsonl`, `stderr.log`, `pgid` | Python | Rust (S2, S3) | None |
| `retained.jsonl`, `launch.json` | none | Rust (S2) | New, additive |
| `supervisor.json`, `supervisor.exit.json` | none | Rust (S2) | New, versioned |
| `attempt.json` (`fabric.attempt.v1`) and sidecars | Python | Python bytes, Rust commit (S3) | None |
| Adapter receipt, `RUN_RECEIPT.json`, manifest | Python | Python bytes, Rust commit (S3) | None |
| `dispatch-owner.json`, `dispatch-status.json` | TS and Python | Rust (S3) | Same fields |
| `cancel.request`, `termination-request.json` | Python, TS | Unchanged writers; Rust reads and writes the same shapes (S4) | None |

Readers that ignore unknown files are unaffected. A launch spec with an
unknown major version is refused with exit 64; during a soak the Python owner
then falls back to the Python supervisor and warns, and afterwards it fails
with `fix: scripts/install-harness`.

## Structural guarantees

| Guarantee | Mechanism | Bug class it removes |
|---|---|---|
| Bookkeeping cannot reach a kill | Only `ProviderGroup::stop(StopReason)` signals. `StopReason` is a closed enum (cancel, watchdogs, `stop` matcher, supervisor signal, post-exit cleanup); I/O errors are a separate `Result` type with no conversion into it, and the writer module never receives the group handle. In S2 the Python owner holds no process at all | #1204: ENOSPC in `publish_contract`, then `diagnostics.write`, then `finally: descendants.stop()` |
| PID and PGID cannot be reused under a signal | The supervisor is the provider's parent and owns its `Child`; the leader is reaped only by consuming the handle, after the last group signal. A zombie leader keeps its PID and group reserved, so `killpg` before reap cannot hit a stranger. Linux additionally signals through a pidfd | PID reuse in reaping and cancellation; start-time string matching (`test_nested_owner_accepts_legacy_inherited_locale_start`) |
| Liveness without identity inference | The custody lock is taken before any record is written and released by the kernel on exit (`O_CLOEXEC`, never inherited by the provider). "Alive" means a non-blocking probe fails. Records are read only to find what to clean up after the lock is free | #1173: orphan reaper killed live runs because a host field was missing; host-liveness probes and permission-denied ambiguity |
| Terminal process record written once | `Lane<Exited>::publish(self)` consumes the state; on error it hands the state back for retry. The file is created by `link(2)` from a fully written temporary file, which fails if the name exists, so a supervisor and a reaper racing to close the same attempt cannot both win | Reaper versus late publisher (`test_reap_attempt_preserves_terminal_publication_after_snapshot`, `test_reap_attempt_skips_busy_publisher_and_releases_run_custody`) |
| The terminal record fits on a full disk | At spawn the supervisor writes and fsyncs a 64 KiB `supervisor.reserve`; on ENOSPC at publication it unlinks the reserve and retries once. Capture and progress writes are best-effort and set `io_errors` | #1204's lost evidence; #1207's "never kill over bookkeeping I/O" becomes a type property |
| Signals and races are ordered events | Signals, child exit, pipe readiness, cancel markers and deadlines all arrive in one loop (kqueue `EVFILT_SIGNAL`, `EVFILT_PROC`, `EVFILT_READ` on macOS; signalfd, pidfd and poll on Linux). The child's signal mask and dispositions are reset before exec. A pure `step(state, event) -> (state, actions)` decides; the shell performs the actions | Signal during `Popen` return, late signal after exit, marker versus natural exit, handler re-entrancy (`test_batch_signal_handler_is_reentrant_while_state_lock_is_held`) |
| One cleanup path | `Drop` for `ProviderGroup` runs the staged stop if the supervisor unwinds; the release profile moves from `panic = "abort"` to unwinding so `Drop` runs on a panic | Cleanup skipped or doubled on exceptions (`test_subreaper_is_released_when_attempt_cleanup_raises`, `test_cancelled_attempt_stops_nested_children_even_when_cleanup_fails`) |
| Nested owners spared by kernel fact | A descendant is spared when it holds the custody lock of a run registered (with today's journal checks) in this attempt's children journal, and that run's `dispatch-owner.json` (written after the lock was taken) names its PID | Forged token, sibling environment, inherited parent identity, owner replaced after enumeration (#1133, #1174, #1185) |

What Rust does not fix: on macOS there is no kernel containment, so a
descendant that calls `setsid` and is reparented before the first census is
still unattributable. The census rules are ported as they are, with typed
`(pid, start_time)` identities. Linux cgroup containment is possible later but
only CI runs Linux, so it is out of scope.

## Test strategy

### Lane conformance suite (the only parity gate)

A black-box pytest suite under `tests/lane_conformance/`, about 15 scenarios,
written in S0 against the Python owner after #1207 merges, then run unchanged
against Rust with `PROVENANT_LANE_IMPL=rust`. It drives a real dispatch with a
fake provider script and asserts only on retained files and the process table
afterwards, never on internal calls or sleeps.

| Scenario | Asserts |
|---|---|
| Clean exit, output, exit 0 and exit 3 | `exit`, capture bytes, no survivors |
| Silent provider | `startup_timeout`, group gone |
| Output then idle; writer touching files while silent | `stalled`; writer tree counts as progress |
| Wall deadline | `wall_timeout`, distinct from idle |
| `cancel.request` before launch, during run, after natural exit | `cancelled` with no pid; `cancelled`; natural status kept |
| SIGTERM to the owner | `interrupted` with `supervisor_signal`, group gone |
| Provider ignores SIGTERM | SIGKILL after the root flush grace |
| Grandchild in a new session, observed before reparenting | Reaped and listed in `reaped` |
| Nested owner in the provider's tree | Spared, counted in `spared`, still running |
| Terminal event with stdin still open (Cursor shape) | stdin closed, settle skipped |
| Capture over the cap | Head and tail retained, `truncated`, result complete |
| ENOSPC on capture, on progress, on the terminal record (fault-injection build) | Provider keeps running; record published from the reserve |
| Owner SIGKILLed mid-attempt | Provider finishes; reaper closes the attempt from `supervisor.exit.json` |
| Two reapers and a late supervisor race | Exactly one terminal record |
| Real-shaped transcripts for each adapter | `stream` matchers give the same terminal, retained and stop results as `parse_output` |

ENOSPC uses a `fault-injection` Cargo feature, built only for the suite. The
suite runs in CI on Linux and on a new macOS arm64 job for changes under
`tools/` (public repositories get macOS runners free); ADR 0027's "no macOS CI
leg" is amended for this crate, because the supervisor's main platform is the
user's Macs. Rust unit tests cover `step()` as transition tables; these replace
the interleaving tests, run in milliseconds and need no processes.

The suite is not deleted when Python goes. Being black-box, it becomes the
Rust lane's contract tests.

### Deletable tests

Measured with `ast` spans (decorators included) for Python and test-block spans
for TypeScript, on `origin/main` at `842958d1`. Counts are test bodies only;
shared fixtures that become unused go too and are not counted. Tests are
deleted when that slice's Python fallback is deleted, not when Rust first
lands.

**S1, front door** (2,763 lines)

| File | Tests | Lines |
|---|---|---|
| `tests/front_door_parity/` (whole directory, including `bench_front_door.py` and `test_check_custody.py`) | all | 2,184 |
| `tests/test_provenant_cli.py`: routing, resolution and help tests (every test whose name lacks `check`) | 21 | 479 |
| `tests/test_check_provenant_install.py`: template cases (`..._stub_matching_the_template`, `..._installed_stub_drift`, `..._template_when_a_crate_exists`, `..._python_front_door_for_the_template`) | 4 | 39 |
| `tests/test_front_door_build.py`: `python_mode_*`, `auto_without_*` | 3 | 12 |
| `tests/test_install_harness.py`: `..._current_template`, `python_mode_rolls_*`, `auto_mode_*` | 4 | 49 |

The 19 `check` custody tests in `test_provenant_cli.py` (444 lines) stay,
re-pointed at the binary, because `check` custody stays in Python.

**S2, `lane run`** (1,818 lines)

| File | Tests | Lines |
|---|---|---|
| `tests/test_provider_exec.py`: census, sparing, reaping, signals, watchdogs, capture, writer progress and subreaper tests (`test_new_session_grandchild_*`, `*_census_*`, `*_spared*`, `*_reaped*`, `test_nested_owner_*`, `test_forged_*`, `*_sigterm_*`, `*_startup_*`, `*_stall*`, `test_wall_timeout_*`, `*_capture_*`, `test_writer_progress_*`, `test_subreaper_*`, `*_grace*`, `test_cancel_allows_provider_session_flush`, `test_cancelled_attempt_stops_nested_children_*`, and the other process-table cases) | 63 | 1,440 |
| `tests/test_dispatch_run.py`: `test_timeout_records_reaped_exit`, `test_sigterm_interrupts_and_reaps_provider_group`, `test_late_signal_after_provider_exit_*`, `test_signal_during_popen_return_*`, `test_external_task_cancel_reaps_only_owned_provider_group`, `test_normal_attempt_records_reaped_new_session_child`, `test_real_dispatcher_restores_signal_handlers`, `test_provider_deadline_is_passed_below_the_owner_deadline` | 8 | 283 |
| `tests/test_cf_dispatch.py`: `test_opencode_dispatch_group_signal_*`, `test_opencode_normal_exit_reaps_grandchild`, `test_interrupted_dispatch_cleans_internal_tempfiles` | 3 | 95 |

**S3, `lane own`** (1,636 lines)

| File | Tests | Lines |
|---|---|---|
| `tests/test_dispatch_run.py`: `test_reap_attempt_*` (10), `test_cancel_cascade_*` (10), `*nested_cli*` (3), `test_record_owner_*` (3), `test_close_mcp_run_*` (3), publication and custody (`test_attempt_publication_waits_for_custody`, `test_terminal_publication_includes_child_registered_*`, `test_terminal_attempt_carries_nested_owner_spared_count`, `test_manifest_append_failure_retains_terminal_attempt`, `test_missing_run_receipt_reports_missing_custody`, `test_dispatch_fails_when_terminal_candidate_*`, `test_batch_child_defers_shared_manifest_append`, `test_counterfeit_active_receipt_*`), owner lifecycle (`test_mcp_owner_closes_receipt`, `test_mcp_batch_cancelled_*`, `test_batch_marker_prelaunch_*`, `test_stale_marker_*`, `test_real_dispatcher_stops_group_*`, `test_concurrent_writer_on_one_worktree_is_rejected`, `test_batch_child_shares_only_a_locked_batch_parent`, `test_standalone_batch_child_replaces_a_dead_owner`) | 45 | 1,424 |
| `tests/test_provider_exec.py`: `test_owner_contract_and_resume_keeps_same_run` | 1 | 114 |
| `tests/test_run_controls.py`: `test_cancel_waits_through_attempt_sidecar_gap`, `test_attempt_terminal_tolerates_receipt_renamed_before_its_digest_sidecar`, `test_cancel_waits_through_late_summary_bundle_completion` (the bundle is complete once `attempt.json` is terminal) | 3 | 98 |

`test_reentry_*` tamper and escape tests stay: they guard reads, which stay
Python.

**S4, `lane status`, `lane cancel`, `lane reap`** (1,886 lines: 821 Python,
1,065 TypeScript)

| File | Tests | Lines |
|---|---|---|
| `tests/test_worker_liveness.py` (whole file, with `worker_liveness.py`) | all | 466 |
| `tests/test_batch_run.py`: `test_real_dispatch_cancellation_reaps_provider_process`, `test_external_batch_marker_cancels_active_child_*`, `test_batch_cancellation_reaps_active_dispatches`, `test_batch_signal_handler_is_reentrant_*`, `test_late_batch_cancel_*` | 5 | 163 |
| `tests/test_run_controls.py`: remaining `test_cancel_*` | 6 | 106 |
| `tests/test_lane_safety.py`: `test_reap_*` (with `scripts/reap.py`) | 11 | 86 |
| `runtime/fabric/tests/process-reaping.test.ts` (whole file) | all | 277 |
| `runtime/fabric/tests/host-liveness.test.ts` (whole file) | all | 91 |
| `runtime/fabric/tests/execution-lifecycle.test.ts`: owner record, custody, kill, orphan reaping and liveness-race cases | 21 | 603 |
| `runtime/fabric/tests/execution-pruning.test.ts`: liveness cases (`...finalising owner is alive`, `never prunes a run still in flight`, `...provider session is live`) | 3 | 94 |

**Total: 8,103 test lines** (7,038 Python, 1,065 TypeScript) across about 330
test functions, most of them the timing, signal and race tests that dominate CI time
and flakes. In exchange the repository gains the conformance suite (about 600
lines, kept) and Rust unit tests (about 800 lines).

Product code removed, approximately: `scripts/provenant` and the template (713
lines); the process half of `provider_exec.py` (about 1,100) and the test-seam
`Popen` branch of `dispatch_run.py` (about 150); owner custody, publication and
reaping in `dispatch_run.py` and `run_lineage.py` (about 850);
`worker_liveness.py`, `scripts/reap.py` and the process-identity code in
`run-registry.ts` (about 1,050). Rust added: about 2,500 lines plus tests.

## Front door

1. **Make Rust the installed front door everywhere.** `install-harness`
   already builds it when `cargo` is present (`PROVENANT_FRONT_DOOR=auto`).
   The workshop Mac has a toolchain, so `auto` installs the binary there; the
   laptop has none and falls back to Python.
2. **Distribution: build on each host with the pinned toolchain
   (recommended).** The user installs rustup once per Mac; `rust-toolchain.toml`
   then pins 1.94.1 on the first build, and the existing digest identity makes
   a stale binary visible. No release pipeline, signing or download trust path
   is needed, and only two hosts exist. `PROVENANT_FRONT_DOOR_BINARY` remains
   for an exceptional host (both Macs are arm64 and the build targets macOS 15,
   so a copy works). The alternative, CI-published per-platform release
   binaries that `install-harness` downloads and verifies by digest and
   artifact attestation, is adopted only if a host cannot carry a toolchain or
   build time becomes a complaint. The lane module adds one crate dependency,
   `libc`, locked in `Cargo.lock`; JSON reuses the existing std-only `json.rs`
   reader plus a small writer. This amends ADR 0027's "no crate dependencies".
3. **Soak:** 14 days with both Macs on the binary.
4. **Delete:** `scripts/provenant`, `scripts/provenant.template`, the
   `python` and `auto` fallback branches in `install-harness`,
   `front_door_build.py` and `check-provenant-install.py`, and the S1 tests.
   Two moves come first: the `check` custody functions that `check_target.py`
   loads from the Python dispatcher move into `check_target.py` itself, and the
   TypeScript tests that run `python3 scripts/provenant` (`run-reader-events`,
   `hosts`) call the owner scripts directly. Without `cargo`, `install-harness`
   then exits 3 with the rustup command as its `fix:` line.

`lane` is a built-in verb, handled in-process like `help` and `root`, and
hidden from `help`. User-facing verbs (`run cancel`, `reap`, `lanes`) keep
their names and route to it in S4.

## Slices

Each slice is one or more pull requests, shippable alone, behind a knob whose
Python path is the fallback until that slice's soak passes. A lane-day is one
implementation lane's working day. The supervision defect baseline is the
fortnight before this spec: about ten fixes or deflakes (listed under Why).

| # | Slice | Knob | Done when | Kill criteria |
|---|---|---|---|---|
| S0 | Conformance suite against the Python owner, after #1207 merges; S2 to S4 interfaces frozen in this file | none | Suite green 50 consecutive CI runs on Linux and macOS | Not deterministic within 3 lane-days: stop the supervisor work (S1 continues), because parity could not be judged |
| S1 | Rust as the only front door | `PROVENANT_FRONT_DOOR` (existing) | Both Macs on the binary 14 days; Python front door and S1 tests deleted | Two soak regressions that force a fallback to Python: keep Python installable and revisit distribution |
| S2 | `lane run` replaces the process half of `provider_exec._run_attempt`, per adapter | `PROVENANT_LANE_IMPL=python\|rust` (default `python`, then `rust` after parity) | Suite green on both platforms for every adapter; 14-day soak on `rust`; Python process code and S2 tests deleted | Parity not reached in 8 lane-days, or soak shows any healthy provider killed or terminal record lost, or more than two Rust-attributable supervision fixes: set the default back to `python` and stop S3 and S4 |
| S3 | `lane own` replaces the `dispatch_run.py` owner; Python split into `plan`, `settle`, `close` | `PROVENANT_LANE_OWNER=python\|rust` | Suite green, including the owner-SIGKILL and reaper-race rows; 14-day soak; S3 tests deleted | Parity not reached in 10 lane-days, or the Python split needs more than about 600 changed lines of glue, or the S2 soak thresholds are breached: stop at S2, which already removes the #1204 class |
| S4 | `lane status`, `lane cancel`, `lane reap`; `run-registry.ts` and `run_controls.py` call them; `worker_liveness.py` and `scripts/reap.py` retired | `PROVENANT_LANE_CONTROL=python\|rust` | Fabric TS holds no process-identity code; graph runner unchanged at its call sites; S4 tests deleted | Not done in 5 lane-days, or any TS path still needs `processMatches`: keep TS control and stop |

Within 7 days of a soak passing, its knob and Python path are deleted; if that
does not happen, Rust is reverted for that slice, so two implementations never
linger. S0 and S1 can run in parallel. S2 starts only after S0. S3 and S4 start
only after the S2 soak is reviewed (see open question 3).

## Other areas assessed

| Area | Rust later? | Why |
|---|---|---|
| Fabric TS runtime (11k lines) | No | MCP SDK and the SQLite store fit TypeScript; after S4 its process code is a call to the binary |
| `model_route.py` (2.7k), `model_route_pools.py`, `provider_quota.py` | No | Policy and catalogue data that change weekly; the tests are data tables, not races |
| `telemetry.py` (1.8k) | No | Batch ingest with no process ownership or latency budget |
| `provider_exec.build_plan` and the confinement profile | No | Security-relevant string building, reviewed as Python; Rust adds nothing structural |
| `fabric_git.py`, `fabric_hosts.py` | No | Subprocess wrappers whose defects are protocol, not supervision |
| `check` custody (`check_target.py`, 70 lines) | Maybe | Would make the front door self-contained; only if it starts to churn |
| `lanes --wait` and `events --follow` (`fabric_lanes.py`) | Maybe, after S4 | A lock-probe wait is simple in Rust; only if wait defects recur |

## Risks

- **macOS has no process containment.** Census attribution stays a heuristic
  for processes that escape before the first census. Rust removes the error
  handling classes, not this one.
- **The test seam hides coverage.** Tests that use the fake `CF_DISPATCH` to
  check owner bookkeeping, not process handling, must move to a fake provider
  behind `lane run` in S2; some may prove to test only the dead branch and be
  deleted outright.
- **Stream matchers may not express every adapter rule.** The per-adapter knob
  keeps such an adapter on Python, at the cost of a longer soak.
- **Two implementations during each soak.** Bounded by the 7-day deletion
  rule.
- **Toolchain drift.** Source changes under `tools/` make installed binaries
  stale until `install-harness` runs; `check-provenant-install.py` already
  says so, and S2 spec versioning refuses a mismatched binary.

## Open questions

1. **Toolchain on the laptop.** Install rustup once on each Mac (recommended,
   no new infrastructure), or should CI publish attested binaries instead?
2. **Soak and kill thresholds.** Are 14-day soaks and the per-slice lane-day
   budgets above acceptable, or should soaks be 7 days?
3. **Gate after S2.** Should S3 and S4 wait for your go-ahead after the S2 soak
   review, or proceed automatically when S2's criteria pass?

## Related

- [ADR 0027](../../adr/0027-rust-front-door.md): Rust front door; amended here
  for the `libc` dependency and a macOS CI job.
- [Provider execution core](provider-execution-core.md): the non-goals this
  keeps.
- [Graph runner](graph-runner.md): consumes run-registry liveness, whose call
  sites S4 preserves.
- [Fabric v2](../fabric-v2.md): attempt states, statuses and the cleanup rules
  ported in S2.
- [Worker liveness](../../../skills/orchestrate/references/worker-liveness.md):
  retired in S4 in favour of `lane status`.
- [#1204](https://github.com/mblauberg/provenant/issues/1204) and
  [#1207](https://github.com/mblauberg/provenant/pull/1207).
