# Agent-native lanes and delivery

Owner: `tools/front-door` records, runtime, CLI and run crates.
Anchor: `tools/front-door/crates/records/src/custody.rs`, `tools/front-door/src/pr/observer.rs`,
`tools/front-door/src/output.rs`, `tools/front-door/src/install/migrations.rs`; the shared
contracts are in sections 2–5.

Why: lanes, progress and delivery need one set of sourced settings, outcomes, deadlines and custody rules.

This is the target contract; implementation is being delivered in batches.

- **Base:** origin/main 656a371d77.
- **Paths:** under `tools/front-door/` unless they start with `docs/`, `config/`, `scripts/` or `.github/`.
- **Chair rulings (binding):**
  - one cut-over per store migration;
  - fleet state is archived, never deleted;
  - queue merge authority defaults to off and is granted by host config only;
  - custody is a lease (#4120).

## 1. Problem and goals

| Defect class | Evidence on origin/main |
|---|---|
| Progress has one driver | `tick` (`src/tick.rs:743`) runs settlement, then graphs, then forge events, serially and with no per-task deadline. `tick_graphs` (`src/fabric/graph_execution.rs:677`) also owns the queue: it reconciles controls (683), checks admission per graph (811) and runs `tick_queues` (883). Only `wait` re-drives a terminal remote attempt. Bad subjects fail tasks on every tick (#4105–#4107). |
| Non-boundary refusals | `crates/run/assets/policy.toml` has 83 `refuse` rows: 16 credentials, 21 other_work, 2 authority, 44 operation. Several are misclassified: prompt-size rows are classed `other_work`, and adapter-capability rows are classed `authority`. |
| Ownership refusals (#4120) | `check_writer_lineage` (`crates/run/src/controls.rs:3732-3815`) refuses a stopped lane's worktree ("continuation worktree belongs to another lane", "has no lane owner"). `Occupancy::Held` becomes `WorktreeError::WriterBusy` (`confinement/src/worktrees/{lifecycle.rs:798, owner.rs:472,612, custody.rs:1417}`, `cli/src/fabric/graph_relay.rs:2293`), which is mapped to `Refused` (`cli/src/fabric/context.rs:1199`). A second live writer is refused even for disjoint work. |
| Lost or buried cause | Root-cause selection exists only in `src/install_report.rs`. Graph progress warnings are strings (`graph/transitions.rs:147`). `gc_result` (`src/worktrees.rs:1159`) exits 3 on a budget stop. |
| Inconsistent verbs | `--timeout` exists only on `wait`. On `run`, `--host` means placement; `upgrade` has both `--hosts` and `--host` (`crates/cli/src/install.rs:54,66`). Queue controls are hidden flags on `stop` and `retry` (`grammar.rs:872-905`). |
| Delivery outside the product | `deliver` keeps hygiene, publish and conflict behind `delivery_execution_wired = false` (`composites.toml`). Policy lives in `fleet.mjs` and `batch.mjs`. |

**Goals (binding):**
- Only the three ADR 0035 boundaries refuse. Everything else is a sourced setting that warns and proceeds.
- "Cannot proceed" is a typed operation error, not a policy refusal.
- Lanes feel like native sub-agents.
- Custody is a lease. Only destroying another live lane's work refuses.
- Plumbing is deterministic and restart-safe.
- Each store has one migration per batch, and there are no shims.
- Names are guessable and output is short.

**Non-goals:** a daemon, a second queue store, an expression language, JavaScript hooks, new hard boundaries.

## 2. Unifying model: five primitives

| # | Primitive | Owner | Contract |
|---|---|---|---|
| P1 | **Setting** | `crates/records/src/provenance.rs` | See the list after this table. |
| P2 | **Check** | `crates/records/src/policy.rs`, `policy.toml` | Rows keep `boundary` and `strength`, and gain an optional `group` column. A `none` row resolves from `policy.<CODE>`, then `policy.<group>`, then `policy.default`, then the row default, using the layer rule from P1. Only `refusable = true` rows can be raised to `refuse`. `operation` means "proceeding cannot work"; such a row never cites ADR 0035 and cannot be overridden. |
| P3 | **Outcome** | `crates/records/src/command_result.rs`, `warning.rs` | The existing `CommandError`, `Warning`, `NextStep` and `Outcome` are extended in place (§3.3). The exit table is unchanged. |
| P4 | **Wake** | `src/fabric/progress.rs` | One custody-checked, deadline-bounded advance of one subject: a graph or a queue item. It reuses the scheduler's control, admission and selection order. Waking never grants resume, retry or landing authority. |
| P5 | **Stage template and effect** | `crates/fabric-runtime/src/graph/template/stages.rs` and `src/fabric/stages/` | Declared stages lower onto the existing compiler and graph mechanisms. Host effects use one durable intent→receipt protocol with reconciliation (§3.9). |

P1 rules:
- **Canonical order.** It is `Layer`'s declared order: `default < profile < detected < host < host_project < project < account < tier < template < route < parent < launch`.
- **Families.** Each family's `Precedence` is a subsequence of that order (a const test).
- **Specificity.** Within one layer, a more specific key wins. Across layers, the higher layer wins. For example, a launch `--checks off` beats a host `policy.CODE=refuse`, and `config explain` shows both.
- **Authority settings** (`domains.pr.land.*`, admin bypass, rulesets) take only `[default, host]`. A value set at any other layer warns `SETTING_LAYER_IGNORED` and is ignored.
- **Declaration.** Every key is declared once in `config/harness-knobs.json`.

Scopes used below: H host, P project, R route, T template, L launch, O owning lane (the lane that created the worktree, applied to that worktree only).

## 3. Per-item design

### 3.1 Progress without tick

Issues: #4104 residue, #4105–#4107.

**Wake.** The signature is `progress::wake(subject: WakeSubject, reason: WakeReason, deadline: Deadline) -> WakeOutcome`.
- `WakeSubject` is `Graph{project,id}` or `QueueItem{project,queue,item}`.
- `WakeOutcome` is `Advanced{transitions}`, `Busy{holder}`, `Idle`, `Skipped{code}` or `OverBudget{cursor}`.

The steps run in this fixed order and are shared by tick:
1. `queue_scheduler::reconcile_controls_before_tick`, scoped to the subject's queue.
2. Queue item: run the scheduler's selection for that item (`tick_queues` claim path), which creates its graph if none exists yet. Graph: `admit_graph_tick`.
3. Take the journal lock with `try_lock`; `Busy` returns at once.
4. Re-read the live revision under the lock and abandon if it moved.
5. `advance_with_budget` under the checkpoint's sealed execution binding, with `budget = deadline.remaining()`.

- **Callers.**
  - `wait` calls `wake` on every wake-up; this replaces `advance_after_remote_terminal`. If its graph worker cannot start, it reports the OS cause and a retry step; the work continues.
  - `result` and inline `run` also call it.
  - `wait --pr N` wakes the queue item bound to that PR.
  - A named `status` or `output` wakes once with `domains.wait.advance_budget_ms`. List `status` stays a snapshot.
- **Lanes.** A lane caller sends `GraphRelay::Wake{subject}` (`crates/cli/src/fabric/graph_relay.rs`). The host runs `wake` as the subject's custody owner. The relay authorises `Wake` like a sibling-session read (line 619): same chair session, no mutation authority granted.

**Deadline contract.** There is one `Deadline` type in `provenant_core` (`remaining()`, `child(cap)`), passed down through the call chain:

| Rule | Behaviour |
|---|---|
| Caps | Task budget, then the per-subject cap, then each I/O call. |
| Blocking I/O | Git and forge calls run as bounded subprocesses through the existing process runner, with `timeout = remaining`. Filesystem work stays in-process and is checked between subjects. |
| Overrun | A task whose worker outlives its deadline reports `over_budget`. It keeps its single-flight lease until the worker actually ends, so the next tick sees `Busy` and never runs the task twice. |
| Fairness cursor | Persisted per task as the last visited `(project, subject)`. It advances past each visited subject whether it was `Advanced`, `Busy`, `Skipped`, `OverBudget` or failed. A subject that was `Busy` or `OverBudget` is retried first on the next pass. |

**Tick.** `tick` keeps its `TaskResult` (`src/tick.rs:49`) and gains a `TickTask` registry.
- **Tasks:** `settlement`, `graphs` (wakes each due subject, oldest wake first, from the cursor), `forge_events`, `gc` (dispatch only) and `steward`.
- **Concurrency:** tasks run on their own worker threads under a `domains.tick.concurrency` cap.
- **Forge events:** the network page fetch is split from local event indexing. Indexing runs after the fetch in the same task, so a slow page delays only `forge_events`.
- **Doc:** `docs/specs/cli.md:127` changes from "graph advancement completes before forge polling" to "graph and forge tasks run concurrently, each under its own deadline". The test changes with it.
- **Placeholders:** `notice_batch_flush` and `quota_refresh` print only "unwired". They are deleted, and their rows are removed from `tick --json`.

**Skip once.**
- **Graphs:** keep the unreadable marker in `read_checkpoint_for_tick`.
- **Projects:**
  - `provenant_config::project_binding` (`crates/config/src/project_binding.rs`) returns `ProjectPresence {Present, Absent{since}, Unreachable{error, retry_at}}`. Today a permission or I/O error is read as present; it now becomes `Unreachable`, retried with backoff and reported once per incarnation.
  - The incarnation is the common-dir device, inode and registration time.
  - Settlement, forge and graph scheduling all read this one classification.
- **Retire.** `provenant retire graph:ID|project:ID` (`custody::SubjectKind` gains `Project`).
  - A graph is archived to `<state>/graphs/<project>/retired/<id>-<ts>/` under the graph lock, then cleared. This changes `retire_unreadable`, which currently unlinks the checkpoint and marker.
  - Its queue item, if any, is parked as `retired`, and its leases are released.
  - A project gets a `retired` disposition fence and is not unregistered. `provenant adopt project:PATH` restores it.
- **#4105 and #4106 are repro-first.**
  - #4105: checkpoint creation already skips false-`when` nodes and eager bind checks that. The slice pins a failing replay first, then fixes the remaining path.
  - #4106: graph, schedule and loop settlement each write the graph pause independently (`graph.rs:3178,3272`, `schedule.rs:1259`). The slice traces every writer and routes all of them through `set_graph_pause_owner`.

| Key | Scope | Default |
|---|---|---|
| `domains.tick.tasks.<task>.budget_seconds` | H | settlement 30, graphs 60, forge_events 30, steward 30, gc 5 |
| `domains.tick.tasks.<task>.enabled` | H | `true` |
| `domains.tick.subject_budget_seconds` | H | 10 |
| `domains.tick.concurrency` | H | 4 |
| `domains.wait.advance` | H/P/L | `on` (`off` makes every reader a pure snapshot) |
| `domains.wait.advance_budget_ms` | H/P/L | 2000 |

### 3.2 Checks warn by default

Issues: #4101 residue, #4103, #3551.

**Audit of all 83 `refuse` rows.** Every row is reclassified with this rule:

| Rule | Class |
|---|---|
| Guards credentials; **destroys** another live lane's refs, commits or uncommitted files (§3.11); or a user gate, merge, release or ruleset | `credentials`, `other_work` or `authority`: refuses |
| Proceeding cannot produce a valid result (an unrecordable prompt, an unsupported confinement claim, a missing object) | `operation`: a typed error naming the cause, not overridable |
| Everything else, including ownership, busy and lineage checks that destroy nothing | `none/warn` with a `group`; `refusable` is kept |

Named results (the slice records the full table in `docs/specs/policy.md`):

| Row(s) | New class |
|---|---|
| `PREPARATION_PREPARE_CANDIDATES_DATA_INTEGRITY`, `ARTIFACTS_RUNSTORE_ARTIFACT_BYTES_DATA_INTEGRITY` (prompt above `RECORD_LIMIT`) | `operation` |
| `PREPARATION_PREPARE_CANDIDATES_AUTHORITY` (capability the adapter lacks) | `operation` |
| `PLAN_CONFINEDPLAN_NEW_AUTHORITY` | Split: claiming unsupported nested confinement is `operation`; a read-only lane enabling extensions or inherited hooks is `none/warn`, group `confinement` |
| `CONFINEMENT_SOURCE_CWD_AUTHORITY` | `none/warn`, group `confinement`. A valid checkout outside the read roots is added to the roots. A missing or replaced checkout is the typed operation error `SOURCE_IDENTITY_MISMATCH`. |
| `REPO_REJECT_ALTERNATES_DATA_INTEGRITY` and `_2` | `none/warn`, group `git`. The snapshot copies borrowed objects into the lane store, then proceeds. Unreadable objects are `operation`. |
| Asset overlap (`source_tree.rs:522`), `SOURCE_TREE_ENSURE_SOURCE_ROOTS_AUTHORITY`, `SOURCE_TREE_ENSURE_PROFILE_ISOLATED_AUTHORITY` | `none/warn`, group `confinement`. Under a pinned view, overlapping grants are re-rooted to copies staged inside the view, never bound to the live tree. If re-rooting is impossible, the grant reads the live tree and warns `LIVE_TREE_EXPOSED` (`policy.confinement=refuse` stops instead). |
| `REPO_VERIFY_WORKTREE_GIT_ISOLATION` | Split by actual custody. `GITDIR_HELD_BY_OTHER_LANE` (other_work) refuses only when repairing would rewrite a registration that a **live** lane's lease names. `GITDIR_REGISTRATION_STALE` (none/warn) runs `git worktree repair` when the backlink names this path and no live lease names anyone else, then re-verifies. Fabricated layouts and mismatched backlinks stay `operation`. |
| `CONTROLS_RUNSTORE_CHECK_WRITER_LINEAGE_AUTHORITY` (`run/src/controls.rs`) | `none/warn`, group `custody`. Replaced by the lease rules in §3.11, with `WRITER_HANDOVER`. |
| `WRITER_WRITERLAUNCH_REQUIRE_CONTINUATION_SOURCE_GIT_ISOLATION`, `CONFINEMENT_WRITER_SOURCE_CONTEXT_AUTHORITY` | A head that moved on from the pinned commit is not destruction, so the continuation re-pins to the current head and warns `WRITER_HEAD_MOVED` (group `custody`). An unreadable predecessor record is `operation`. |
| `WRITER_WRITERLAUNCH_SNAPSHOT_GIT_ISOLATION` | `operation` (the HEAD is not on the selected branch, so the snapshot would be wrong), naming the branch and the HEAD. |
| Hard links: `permissions/anchors.rs:191`, `validation.rs:73` | `none/warn` except for credential stores. Settlement already moves hard-linked handoffs aside (`run/src/completion.rs:153,233`), so it is unchanged. |

**Peer projection** (`peer_project_unprojectable`, `cli/src/fabric/context.rs:956`) becomes the setting `domains.fabric.project_projection = auto|locate|mirror` (H/P/L, default `auto`).

| Mode | Behaviour |
|---|---|
| `locate` | Today's home-relative `ProjectLocator`. |
| `mirror` | **Readers only.** Creates a snapshot commit of the working tree (tracked and untracked, non-ignored) without touching the index, bundles exactly that ref and sends it to `<peer state>/mirrors/<origin-host-id>/<project-id>`. The peer verifies the bundle's object ID before binding. The lane sees a snapshot, not live edits, and `PEER_SOURCE_SNAPSHOT` says so. |
| `auto` | `locate` first, then `mirror` for readers. A writer that cannot locate the project fails with the operation error `PEER_PROJECT_UNLOCATABLE`. It names the key and `next: run without --host`. |

A failed transfer is `Transient{retry_at}`. A verification mismatch is `operation`.

**Strictness.**
- `--strict` sets `policy.default=refuse` at the launch layer. It affects only `refusable` `none` rows.
- `--checks off` sets `policy.default=off`.
- `CHECK_NOT_OVERRIDABLE` fires only for an explicit `policy.<CODE>` on a boundary or operation row, never for `default` or a group.
- Policy keys live where `policy.<CODE>` lives today: host `config.toml [policy]`, `.provenant/policy.toml`, and dispatch.

**Other residue.**
- `lanes.pr` default changes from `never` to `allow`. The deliver template sets `never` at the template layer, because the queue owns publishing.
- `VerificationResolution.conflict` (`run/src/verification.rs:47`) is deleted. The brief wins with `VERIFICATION_BRIEF_OVERRIDES`.
- `lanes.sandbox` keeps its default `auto`, unchanged.
- `LANE_DEPTH` already warns, so nothing changes.
- **#4103:**
  - `explain_source_admission` evaluates a peer-placed writer against the peer plan.
  - `verify_smoke_with` and `preview_smoke` share `smoke_argv()`.

No migration is needed: the `policy.<CODE>` keys and their values already exist.

### 3.3 Error and warning contract

Issues: #4102 residue, #3777.

```rust
// crates/records/src/command_result.rs
pub struct Subject { pub host: Option<String>, pub project: Option<ProjectId>, pub graph: Option<String>, pub node: Option<String>,
    pub attempt: Option<u32>, pub item: Option<String>, pub pr: Option<u64>, pub head: Option<GitOid> }
pub struct SettingRef { pub key: String, pub value: String, pub layer: provenance::Layer, pub origin: String /* file:line or flag */ }
pub enum ErrorClass { Boundary(policy::Boundary), Configured, Operation, Transient { retry_at: Option<jiff::Timestamp> } }
pub struct CommandError { pub code, pub message, pub cause: Option<String>, pub class: ErrorClass, pub subject: Option<Subject>,
    pub setting: Option<SettingRef>, pub root: Option<Box<CommandError>>, pub next: Vec<NextStep>, #[serde(flatten)] pub diagnostic }
// NextStep { argv: Vec<String> /* may be empty for a condition */, why: String }
// Warning keeps subject: Option<String> for display, and gains subject_ref: Option<Subject>, setting: Option<SettingRef>, next: Vec<NextStep>.
```

- **`repair` is deleted** from both `CommandError` and `Warning`, and `next` replaces it.
  - The signature of `CommandError::new(code, message, cause, repair)` is kept, so the 49 call sites do not change. Its `repair` string becomes `NextStep::from_text`: argv when it parses as a `provenant …` command, otherwise a condition.
  - `Warning::repair()` stays as a builder that pushes to `next`.
- **`CommandResult.next`** is the one list rendered. On error it holds `error.next` followed by the command's own distinct steps; an identical step appears once.
- **Builders.** `CommandError::with_subject`, `with_setting` and `with_class` are added. `class` defaults to `Operation`.
- **Tests.** A catalogue test requires `setting` for every `Configured` error and every `none`-row warning, and requires a non-empty `next` on every error.
- **Root cause.**
  - `fabric_runtime::graph::root_cause(&GraphCheckpoint) -> Option<RootCause{node, attempt, code, message}>` walks past `upstream_blocked` dependants.
  - Callers: `status`, `wait`, `run`, `upgrade --verify` and queue parks.
  - The selection in `install_report.rs` is deleted.
- **Forge quota.** `pr::observer::ObserveError::RateLimited{reset}` becomes `Transient{retry_at}`. `run review --pr` reports `FORGE_QUOTA_EXHAUSTED` instead of `INVALID_INPUT`.
- **Host clock.** A clock read failure during a run-store call converts into `RunError::Input` with `ErrorKind::Time`, so it reaches the CLI as a stable `TIME` code with its leaf cause instead of failing the error conversion.
- **#3777.** These setting-origin enums and structs are deleted in favour of `provenance::{Layer, Sourced}`. Each is verified on origin/main:
  - `policy::Source` (`records/src/policy.rs:54`);
  - `PacingSettingSource` and `PortSettingSource` (`records/src/diagnostic.rs:314,340`);
  - `lane_contract::Sourced` (`records/src/lane_contract.rs:65`) and `pr::landing::policy::Sourced` (`src/pr/landing/policy.rs:74`);
  - `PolicySource` and `ConfinementSource` (`confinement/src/permissions/{policy.rs:622, resolved.rs:36}`);
  - `DecideTimeoutSource` (`graph/asks.rs:151`), and `CiLimitSource` and `InputCapSource` (`graph/model.rs:1450,2336`).
  - Data-origin enums are kept, for example `IdentityEvidenceSource`, `UsageSource`, `PriceSource`, `BriefSource`, `WeightSource` (config or catalogue), `LandingSource`, `WriteSource`, `WriterBaseSource`, `OfferSource` and `PriorSource`.
  - `QUEUE_KNOB_PRECEDENCE` and `seal_queue_knob(s)` give way to the generic `seal_settings` over a map of shipped defaults.

Human form:
```
error [PEER_PROJECT_UNLOCATABLE] smoke-7f3a/task--read#1 on workshop: project is outside the peer's home
  setting: domains.fabric.project_projection = locate (project .provenant/config.toml:12)
  next: provenant run task --brief … (without --host)
```

### 3.4 Consistent verbs and flags

`CommonArgs { host: Vec<HostArg>, timeout: Option<DurationArg> }` is flattened into these verbs: `status`, `output`, `result`, `wait`, `events`, `gc`, `run`, `upgrade`, `ci`, `retire` and `queue`. `--json` and `--full` stay global. The exempt verbs are `help`, `hook`, `internal`, `declare`, `notes` and `config`; that list lives in the catalogue test.

Peer host-read and GC responses carry the full command envelope in `data`, alongside the `result` variant tag; this keeps the tag distinct from the payload and preserves command outcomes across hosts.
Session-stream host reads carry their JSON lines as an ordered `stream` array in the command data; invalid JSON fails the read.

| Flag | Meaning |
|---|---|
| `--host H` (repeatable, `all`) | The host the command acts on. For reads (`status`, `output`, `result`, `events`) the selected host serves the read through typed `PeerAction::HostRead`; all command options and the result data are preserved. `output --final` pages over that action and validates the sealed attempt, digest and contiguous offsets before writing `--to`. `run` uses it for placement, as today, with exactly one host. `gc` uses `PeerAction::Gc`. On `upgrade` it names the hosts to upgrade (default `all`); the old `--hosts` is deleted and smoke placement moves to `--verify-host`. |
| `--host all` aggregation | Named hosts, repeated host names and `all` fan out `status`, `output`, `result`, `events` and `gc`. Human output prints one section per host. JSON is `data.hosts: [{host, status, data?, error?}]`. The exit code is the worst per-host outcome, including `Unavailable` for an unreachable host; when at least one host answered, an unreachable peer also adds `PEER_UNREACHABLE` and `partial:true`. Event consumers advance only through records represented in an aggregate successfully delivered to stdout. Multi-host `run` is `Invalid`. The hermetic `run smoke` lifecycle applies the same rule and passes a single named host to its verification graph. |
| `--timeout DUR` | Accepts `90s`, `10m` and `2h`. It bounds the whole invocation as a `Deadline`, and nested calls get what remains. Defaults come from `domains.cli.timeouts.<verb>`: status/output/result/events 30s, gc 5m, ci 2m, run (inline) 30m, wait `domains.wait.timeout_seconds`, upgrade 20m. The value is clamped by `domains.wait.max_timeout_seconds`, which warns once. A mutation that times out reports its operation ID and whether it is `committed|pending|uncertain`. |

`config explain KEY` is a new `Config::Explain` subcommand. It prints the winner and every losing candidate with layer, origin and specificity, using `crates/config/src/inspection.rs`.

### 3.5 Short output

Issues: #4092 residue, #4099, #3775.

- **Collapse.** The existing warning sink and the bounded renderer (`src/output.rs`, `crates/cli/src/render.rs`) collapse warnings by `(code, setting.key, message template)`. The line reads `warning [CODE] ×7: <first> (first: <subject>) — --full lists all`. JSON keeps up to 64 instances plus `warnings_omitted`. The key is `domains.cli.collapse_warnings` (H/L, default `on`).
- **Pool-selection warning.** `apply_route` (`graph/transitions.rs:141-176`) checks only identifier fields against `HOST_ID_BYTES`.
- **Typed graph warnings.** `NodeProgress.warnings` becomes `Vec<Warning>`, which affects the model, journal, relay and renderers. Launch warnings retain their registered code and source; the graph-store transform migrates older strings (§3.10).
- **Exits.** The existing table is unchanged (`Outcome::exit_code`):

| Case | Outcome | Exit |
|---|---|---|
| Success with warnings | Ok | 0 |
| A budget stop that saved a cursor (gc, reclaim, tick) | Ok, with `partial:true` and a warning naming the budget key, `next` resumes | 0 |
| Inline `run` past `--timeout` with the lane still running | Timeout, the same as `wait`, with `data.still_running = true` and `next: provenant wait NAME` | 124 |
| A silent lane | Stalled | 6 |
| An oversized result | The real outcome is kept, plus `RESULT_TOO_LARGE` | unchanged |

- **Oversized results.** A quota show result that exceeds the envelope cap is paged with its original successful outcome and a `RESULT_TOO_LARGE` warning; filtering remains available. `RESULT_TOO_LARGE` is a warning on its code paths; `docs/specs/cli.md:162` is corrected to match. The full result is also written to `<state>/reports/<verb>-<ts>.json` (mode 0600, the same redaction as the envelope, pruned by gc after 7 days), and the warning carries the path. The bounded envelope's `data.artifact` is an `AbsolutePath` wire object naming that same report.
- **Compact gc and upgrade.** `gc` prints per-class aggregates (under 32 KiB) and `--full` adds per-candidate rows. `upgrade` prints one release line, and `--full` adds the file counts and notices.
- **Reclaim.** `trait Reclaimable {class, scan(cursor, deadline), apply(plan, deadline)}` lives in `crates/run/src/reclaim.rs`.
  - Its cursor is `ReclaimCursor{page_start_seq, page_end_seq, visited}`, and a resume finishes the current page before it advances (#4099).
  - Attempt scratch moves first.

### 3.6 CI-produced patches for generated artefacts

- **Registry.** `config/ports.json` (`.provenant/ports.json` links to it) gains `generated: [{name, generate: [argv], cwd, outputs: [globs], tracked: bool}]`.
  - `swift-fixtures` and insta snapshots are `tracked = true`.
  - `reference` is `tracked = false`: it is render-only, so its job keeps rendering and uploading `contract-reference` and never produces a patch.
- **Drift.** `scripts/check-generated` runs every tracked generator. It captures their output with a scratch index (`GIT_INDEX_FILE`, `git add -A -- <outputs>`, `git diff --cached --binary HEAD`), which includes additions, modifications and removals, and never touches the real index. A failed generator is recorded and the others still run. `generate-swift-fixtures` already collects every mismatch; it now also reports added and removed fixtures.
- **CI job.**
  - The new `generated` job in `.github/workflows/ci.yml` replaces the swift-fixtures step at :1005-1012.
  - `generated-reference` stays as it is.
  - The aggregate CI gate depends on `generated`.
  - The job uploads `generated-<head_sha>`, containing `patch` and `manifest.json`. The manifest is `GeneratedManifest{repo, workflow, run_id, run_attempt, head_sha, generators:[{name, status: clean|drift|failed, files}], patch_sha256}`.
- **CLI.** The grammar matches `ci failures`: `provenant ci patch pr N | run ID [--apply] [--worktree W] [--commit]`.
  - **Checks before applying:** repo, workflow and run attempt; the manifest head against the worktree HEAD; the digest. A mismatch is the boundary-free `Configured` error `CI_PATCH_HEAD_MISMATCH`, naming both heads and a `next`.
  - **Applying:** `git apply --check` runs first, so dirty work is preserved.
  - **Committing:** `--commit` uses a pathspec limited to the patch paths.
- **Key.** `domains.ci.generated_patch = off|offer|apply` (H/P/T, default `offer`). The deliver template sets `apply`.

### 3.7 Review convergence

Issues: #4017 wiring, #4108, #4109.

**Finding ledger.** `FindingLedger` is stored per queue item and keyed by head. It holds findings, unresolved findings, their dispositions, coverage per lens and per invariant, and `last_reviewed_head`. Each displayed finding ID retains its structural source identity so a later colliding raw ID cannot rename or merge an existing ledger item. Every round reads and writes it, and the adjudicator writes its verdict and findings into it. Repair always reads the ledger, never one reviewer's output.

**Severity.** `Severity {Nit, Low, Medium, High, Blocking}` decodes the existing string field (`model/finding.rs:51`):
- `critical|blocker|blocking` → Blocking;
- `major|high` → High;
- `medium|moderate` → Medium;
- `minor|low` → Low;
- `nit|info|style` → Nit;
- anything else → High, with `FINDING_SEVERITY_UNKNOWN`.

An unresolved finding whose existing blocking class is set blocks acceptance regardless of `accept_below`.

| Key | Scope | Default | Meaning |
|---|---|---|---|
| `domains.review.later_rounds` | T/H/P/L | `delta` | Round 2 onwards reviews `last_reviewed_head..head`, plus the ledger's unresolved findings and invariants. Only the lenses with open findings run again. A moved base, an unattributable delta, or an invariant whose only lens was unavailable widens to `full`. |
| `domains.review.adjudicator_route` | T/H/P/R/L | `principal` | Distinct member verdicts on one head, including approval beside inconclusive, or contradictory blocking findings, get one `review.adjudicate` lane keyed by (review-chain item, head). It uses no round. |
| `domains.review.accept_below` | T/H/P/L | `high` | Accept when CI is green on the exact head, no blocking-class finding is open, and every open finding is below this severity. The lower findings are posted. |
| `domains.review.evidence_retries` | T/H/P/L | 2 | Missing or malformed evidence re-runs the same lens on the same route. It never leads to repair. |
| `domains.review.lens_retry` | T/H/P/R | 1 | A lens that ends `blocked_environment` or `model_unavailable` reruns on the next eligible route, then becomes `Unavailable`. |
| `min_successful` (panel) | T | lenses − 1, at least 1 | Reduce over the completed lenses and post with `coverage_gaps`. An invariant with no completed lens is uncovered, which makes the round `INCONCLUSIVE`. |
| `routing.pin` | H/P/R/L | `family` | Fallback on `model_unavailable` stays within the tier, the family, or nothing (`exact`), and records `fallback{from,to,cause}`. Only `exact` refuses. |

When a later panel for the same review-chain item and head settles with a split
after its adjudicator has succeeded, continue that same adjudicator node with
the union of known split panels. Retain the old attempt and a receipt of its
pinned head, prior verdict, output digest, the panels consumed by that verdict,
the newly added panel IDs, and the complete prior output snapshot as separate
fields; journalled expansions retain each attempt's full input union. The
snapshot remains in the receipt even when the provider gives no immutable
attempt reference, and its content-addressed evidence uses the checkpoint's
32 MiB blob cap. Clear the old verdict before retrying, and require fresh
adjudication before downstream gates proceed.

Before admission, every split panel must have succeeded with a usable verdict,
complete findings and no partial output. The adjudicator must return each full
input finding under its qualified identity with an explicit disposition. PASS
is invalid while a blocking finding remains, including boundary and data-loss
findings until evidence falsifies them.
Journal the continuation so replay reuses the same node and evidence union;
never accept the superseded verdict.

`domains.routing.review.*` keeps lens selection, which is routing. `domains.review.*` holds review behaviour.

### 3.8 Concurrency-safe upgrade and verify

Verify already has unique graph names, home-relative scratch and per-binding cleanup (`src/install.rs:2180-2264`). The remaining gaps:

- **Install.** After it takes `install.lock` (`crates/config/src/managed.rs:1550`), the install compares the receipt's `(version, artefact digest)` with the target. On a match it skips to verify with `UPGRADE_ALREADY_INSTALLED`. The wait is bounded by `domains.install.lock_wait_seconds` (120) and names the holder's PID, start time and version.
- **Pin.** Verify takes a shared `<state>/releases/<version>.lock`. Install and swap never remove a release whose lock is held, and gc skips it.
- **Keep.** `--keep` writes `<smoke>/keep`. gc and peer cleanup honour it, and `next` names `provenant retire graph:<smoke>`.

### 3.9 Delivery queue absorbs the fleet

Issues: #3972 residue, #3510.

**One CI reader.**
- `pr::observer::observe_head(repo, pr, expected) -> HeadReading` returns `{expected_head, branch_ref, pr_head, mergeable, required: Vec<Check>, observed_at, complete: bool, pending_reason?, retry_at?}`.
- A moved or missing pull head stays pending and retains the observed head and branch without requiring checks for the expected commit.
- It is `complete` only when all of these hold:
  - every page has been read;
  - every read saw the same `pr_head`;
  - the required set comes from branch protection or the ruleset and is non-empty;
  - no required check is queued or in progress;
  - `observed_at` is within `max_age` (60s).
- A GraphQL→REST fallback must report the same head, or the reading is pending.
- These consumers move to it: `pr/wait.rs`, `pr/landing/gate.rs`, the graph CI gate, `pr/status/batch.rs:318` and `pr/status/ci.rs:333,433`.

**Effect protocol.** This is settled in batch 2 slice 0, and every effect implements it.

| Part | Contract |
|---|---|
| Identity | `OperationId = (item, stage, attempt, generation)`, stable across retries of the same step. |
| Ledger | `<queue dir>/items/<item>/effects.jsonl` is append-only. An `Intent{op, kind, subject, expected_head, params}` is written and fsynced before execution, and the `Receipt{op, state, observed}` after it. |
| Lock order | The queue registry lock, then the item lock, then the graph journal lock, never reversed. The effect runs with only the item lock held. |
| States | `Intended → Submitted → Confirmed | Failed | Uncertain`. A crash or timeout after submission is `Uncertain`. |
| Reconcile | Runs before any new intent for the item. Push: the remote ref equals the intended head. PR create: search by head branch. Merge: the PR is merged at the expected head (the merge used match-head). CI rerun: the run ID's attempt count. |
| Generation | `hold` bumps the item generation. An intent whose generation is older than the item's cannot be submitted. Effects already submitted stay visible as `holding (1 effect uncertain)` until reconciled. A hold never claims to undo an effect. |
| Landing | Reuses `src/pr/landing`. It merges only when CI is complete and green, acceptance holds and mergeable is clean, all on the same (repo, PR, head), with `--match-head-commit`. |
| Authority | Merging integration-branch slices is a writer operation on an owned branch and needs no landing authority. Landing into a protected branch needs `domains.pr.land.enabled` (H only, default `off`). |

**Records.**
- `QueueItem` gains these fields, all `#[serde(default)]`:
  - `priority: Sourced<i32>`;
  - `generation: u64`;
  - `hold: Option<Hold{generation, reason, by, at}>`;
  - `phase_used: BTreeMap<Phase, u32>`;
  - `baseline_head: Option<GitOid>` and `baseline_source: recorded|migrated`;
  - `expected_head`;
  - `rung`;
  - `entry: Option<StageId>`;
  - `batch: Option<BatchState>`.
- `queue_control::Action` gains `Hold` and `Prioritise`.

**CLI.** Controls live under `provenant queue`, and the hidden `--queue-control/--queue-revision/--operation-id` flags on `stop` and `retry` are deleted.

| Command | Behaviour |
|---|---|
| `provenant run deliver --over issues:N,M [--priority P]` | One item per issue. |
| `provenant run deliver --adopt pr:N|branch:B` | Sets `entry = conflict`, and records a new lineage with `baseline_head` set to the observed head. A branch whose last lane is terminal is adopted under §3.11 with `WRITER_HANDOVER`. |
| `provenant run deliver.batch --batch FILE` | One batch item; the file schema follows. |
| `provenant queue hold ITEM [--reason]` | Bumps the generation (see the protocol). |
| `provenant queue resume ITEM [--stage S]` | Clears the hold, resets the resumed phase's budget and continues. |
| `provenant queue prioritise ITEM P` | Sets the priority. |
| `provenant queue accept|fix --findings FILE|drop ITEM` | The existing ledger actions as verbs. |
| `provenant queue status [--parked]` | The board: item, issue/PR, stage, round and rung, hold, lease, head, next wake, park reason, a bounded handoff and `next`. `--parked` is the needs-chair digest. |

ITEM is an item ID, `issue:N` or `pr:N`, parsed by `QueueTarget`. Graph `stop` and `retry` are unchanged.

Batch file:
```toml
integration_branch = "epic/e1-agent-native"
base = "origin/main"
contract = "docs/specs/epic-e.md#slice-0"      # read by every slice brief
[slice0]
brief_file = "briefs/s0.md"
owns = ["crates/records/src/command_result.rs", "..."]
[[slice]]
id = "s01"
deps = []
brief_file = "briefs/s01.md"
owns = ["..."]
tests = ["name", "..."]
route = "junior"                 # optional
```
Owned paths must be disjoint across the slices that can run in parallel. An overlap warns `BATCH_OWNS_OVERLAP`, naming both slices, and the compiler puts the later slice after the earlier one (`policy.BATCH_OWNS_OVERLAP=refuse` stops instead). This follows the advisory-claim rule of §3.11.

**Fleet mapping.**

| Fleet behaviour | Product mechanism |
|---|---|
| daemon and `state.json` | the graph store, the queue registry and the effect ledger, advanced by tick and by `wake` from readers |
| `ciTake` before push | `publish` takes the `ci` admission lease before its intent. It releases the lease on conclusive CI, conflict diversion, hold or exit. |
| `rerunIfCancelled` | the `ci` gate reruns a cancelled run internally (`policy.rerun_cancelled`), without spending a round |
| exact head, self-push, push lag | `HeadReading`. Progress means head ≠ `baseline_head`, whoever pushed. |
| close/reopen resync | `policy.head_binding.resync = reopen`, once, after `resync_after`, recorded as its own effect |
| quota as a wait | `Transient{retry_at}` holds the stage |
| conflict before CI, merge not rebase | the `conflict` gate precedes `ci`; zero checks re-test mergeability; `conflict_strategy = merge` |
| phase budgets, chair reset | `[budgets]`; `queue resume` resets the resumed phase |
| prep refusal reconcile | `write` outcome `prep_refused` leads to `reconcile`, then a relaunch |
| relaunch on a stopped writer's branch | §3.11 handover; no special flag |
| empty excerpt | `ci.failures` must be non-empty and clustered, or the outcome is `operation_error` |
| wide repair | a ladder rung with `min_failures = 10` and `brief = "wide"` |
| WIP on timeout | `lanes.writer_dirty = wip_commit` (existing) |
| `hooks.mjs` | template layers, plus `command` stages |
| `needs-chair.md` | `queue status --parked` and the inbox |
| `batch.mjs` | `deliver.batch` |

### 3.10 Store migrations

One cut-over per store per batch.

**Store-owned transforms.**
- Each store owner exposes `fn migrate(from: u64, to: u64, record) -> Result<record>`, which is pure and deterministic, plus a `migrate_store()` that runs it under that store's own lock.
  - Graph journal: `journal.rs:2313`, which rehydrates, re-externalises and re-digests.
  - Queue registry: `queue_registry.rs:259` (`REGISTRY_SCHEMA`).
  - Config: a knob rewrite.
- Each store writes `migrating-<to>` before it starts and `schema_version=<to>` after. A store found with `migrating-<to>` resumes the transform, which is idempotent per record.

**Cut-over** by `provenant upgrade`, which already moves every host in one run:
1. Install the new binary.
2. Quiesce: disable the tick schedule, then wait up to `domains.install.quiesce_seconds` (300) for graph, queue and lease locks to drain. If they do not drain, refuse with `UPGRADE_NOT_QUIESCENT`, which lists the holders, and migrate nothing.
3. Migrate graph, then queue, then config, recording each step ID in the install receipt (`install/migrations.rs`).
4. Re-enable tick.

**Old binaries.** An old binary that opens a newer store refuses (the existing strict-schema read). Peers exchange their binary version, and a mismatch is `Transient` with `PEER_UPGRADE_PENDING`. No mixed-version reader exists.

| Batch | Store | Transform |
|---|---|---|
| 1 | config | `lanes.isolation=shared` becomes `lanes.read_source=live`; `private` is dropped, because the destination rules replace it. `lanes.pr=never` set by the user is kept. Persisted source tags (`shipped_default`, `bundled` and `builtin` become `default`; `dispatch` becomes `launch`). |
| 1 | graph | `NodeProgress.warnings`: each string becomes `Warning{code: LEGACY_WARNING, message}`. Sealed `accept_inconclusive_without_blocking = true` becomes sealed `accept_below = blocking`, and `false` becomes `off`. Sealed `lanes.isolation` becomes `read_source` as in the config row. Source tags as in the config row. |
| 1 | run store | Each sealed `writer-owned.json` (`WriterOwnership{lane_owned, owner}`, `crates/run/src/writer.rs:4216`) is rewritten to `lease.json` (`WorktreeLease{holder, since, sharing: Inherit}`). An unowned record has no lease. The lease state is derived from the holder's attempt state, not stored, so a terminal holder is already a lapsed lease. |
| 2 | queue | Header 2 becomes 3. `baseline_head` is the item's current recorded head, with `baseline_source = migrated`. `generation` is 0, there is no hold, and the items' `FindingLedger` starts empty, with existing findings imported as open. |

Tests: old fixtures per store, an interruption between stores, replay on the new binary, an old binary refusing a new store, and `UPGRADE_NOT_QUIESCENT`.

### 3.11 Worktree and branch custody as a lease

Issues: #4120.

**Lease.** A worktree and its branch carry `WorktreeLease{holder: AttemptId, since, sharing: Inherit|Allow|Warn|Refuse}` (`crates/records/src/custody.rs`). It replaces `WriterOwnership` as the ownership record.
- The lease is **live** while the holder's attempt is nonterminal. It **lapses** when the holder is terminal: done, stopped, failed or cancelled.
- It is derived from the attempt state on every read, so no sweeper is needed and a crash cannot leave a stale lease. A holder whose host is unreachable counts as live until `domains.fabric.lanes.lease_unreachable_seconds` (900), then as lapsed, with `LEASE_HOLDER_UNREACHABLE`.

**Continuing a lapsed lease.** Any lane may launch on a worktree or branch whose lease has lapsed, with no special flag. This covers `--worktree`, `--branch`, `retry`, `resume`, queue relaunch and `--adopt`.
- The launch takes the lease and warns `WRITER_HANDOVER`, naming the prior lane, its terminal state and its last head. It records a `Handover{from, to, at, head, uncommitted: [path]}` fact in the run store.
- Uncommitted files left by the prior lane are kept in place, listed in the warning and the new lane's brief footer, and never reset, stashed or discarded.
- A head that moved since the prior lane's pin is re-pinned to the current head with `WRITER_HEAD_MOVED` (§3.2).
- This replaces every refusal in `check_writer_lineage` (`controls.rs:3678-3815`): "graph predecessor context", "continuation owner attempt missing", "continuation worktree belongs to another lane" and "continuation worktree has no lane owner". `RunError::Nonterminal` from this path becomes the live-sharing rule below. `RunError::WriterLineage` is deleted, and an unreadable predecessor record becomes `operation`.

**Live sharing.** A second live writer in a worktree whose lease is live follows `lanes.worktree.sharing`:

| Value | Behaviour |
|---|---|
| `allow` | Proceeds silently and records the co-writer. |
| `warn` (default) | Proceeds with `WORKTREE_SHARED`, naming the holder, its state and the setting. |
| `refuse` | Refuses with `WORKTREE_SHARING_REFUSED` (a `Configured` error), naming the holder, the setting and its source, and `next`: wait for the holder, or launch with `--destination new`. |

- **Scopes:** H/P/L, plus O. The owning lane sets `refuse` for its own worktree by launching with `--worktree-sharing refuse` or by calling `provenant lane share refuse` inside the lane. This is stored in its lease, and it wins over the other layers for that worktree only. That is the #3453 "owning lane decides" rule.
- **Mechanism:** `Occupancy::Held` no longer maps straight to `WorktreeError::WriterBusy`. `lifecycle.rs:798`, `owner.rs:472,612`, `custody.rs:1417` and `graph_relay.rs:2293` call one `custody::admit_writer(tree, lease, sharing) -> Admit{Proceed{warning?}, Refuse}`. `WriterBusy`, and `FacadeError::WriterBusy` with its `Refused` mapping (`context.rs:1199`), remain only for `Refuse`. The nested-provisioning refusal at `graph_relay.rs:310,346` ("nested provisioning belongs to another lane") follows the same rule.
- **Path claims are advisory.** `--write-scope` and `path_claim` are recorded and shown to every co-writer. An overlap with a live claim warns `PATH_CLAIM_OVERLAP`, naming both lanes, and refuses nothing.
- **Git safety:** git's own index lock and ref compare-and-swap protect concurrent commits. A commit that loses the index lock retries up to `domains.fabric.lanes.index_lock_retries` (5) with backoff, then fails `Transient`.

**Only destruction refuses.** `custody::guard_destructive(op, tree|ref) -> Result<(), CommandError>` runs before every operation in this list, wherever Provenant performs it (worktree lifecycle, gc, landing cleanup, retry reset, `ci patch`, queue effects):
- reset or checkout that would overwrite files another live lane has uncommitted;
- stash or clean over those files;
- force-push, or any non-fast-forward ref update, to a branch whose lease is live with another holder;
- deleting that branch or removing that worktree.

The guard refuses as `other_work` with `DESTROYS_LIVE_WORK`, naming the holder, its state and the files or ref, and `next: provenant wait HOLDER` or `provenant stop HOLDER`. Adding commits, fast-forward pushes and edits to other paths are not destruction. A lapsed lease never triggers the guard, but its uncommitted files are still kept: gc moves them to `<state>/retained/<attempt>/` before removing a worktree.

| Key | Scope | Default | Values |
|---|---|---|---|
| `lanes.worktree.sharing` | H/P/L/O | `warn` | `allow|warn|refuse` |
| `domains.fabric.lanes.lease_unreachable_seconds` | H | 900 | seconds |
| `domains.fabric.lanes.index_lock_retries` | H/P | 5 | count |

The "one writer per worktree" rule in `skills/orchestrate` and `docs/specs/lane-contracts.md` is replaced by this section.

## 4. A lane as a native sub-agent

These are two separate defaults, and neither implies the other.

| Aspect | Default | Opt-in per launch |
|---|---|---|
| **Context** (what the model is told) | **Fresh**: the brief plus the standing preamble (`lanes.brief_preamble`, `--preamble-file`). Never the parent's conversation, transcript or notes. | `--context FILE` (repeatable) and `--notes NAME` (repeatable, shared notes). Both are inlined and together bounded by `lanes.context_max_bytes` (256 KiB); content above the bound is cut with a warning. |
| **Working tree** (what the lane can open) | **The parent's checkout or worktree, read in place.** Files are read on demand and nothing is copied into the prompt. Readers see uncommitted edits live. | `--source default_branch|REF` pins an immutable view. `--worktree PATH` or `--branch B [--base R]` gives a writer tree, owned or continued under §3.11. |

**Writers.** Custody is the §3.11 lease. Placement uses the existing shared-writer grant (`graph_relay.rs:148 bind_shared_writers`) and `ChildLaunchRequest{destination, path_claim, commit}`.

| Case | Destination | Commits |
|---|---|---|
| A writer launched inside a lane, with `lanes.destination.write = auto` | `ParentWorktree`, through the owning lane's sealed grant, subject to `lanes.worktree.sharing` (default: proceeds with `WORKTREE_SHARED`). An overlapping live claim warns `PATH_CLAIM_OVERLAP`. | **The child does not commit by default.** Its handoff lists the changed paths with their digests, and the parent commits, as with a native sub-agent. `--commit own` makes the child commit only its own paths, with its own index and a compare-and-swap ref update (`update-ref <ref> <new> <expected>`); a lost race is `Transient` and retried. |
| The owning lane set `sharing = refuse` | `ManagedWorktree` from the parent's HEAD, with `LANE_WRITER_SEPARATED` (warning); `--destination parent` refuses `WORKTREE_SHARING_REFUSED` | its own branch |
| A writer launched by the chair | `ManagedWorktree` from the chair's HEAD | its own branch |
| `--worktree` or `--branch` naming a lapsed lease | that tree or branch, with `WRITER_HANDOVER` | its own commits on that branch |

**Carrying uncommitted edits.** `lanes.carry_uncommitted` (H/P/L, default `tracked`, values `off|tracked|all`) applies only to local writers when the new worktree branches from the chair's HEAD with no explicit `--base`. A peer-origin launch requesting carry warns `CARRY_PEER_UNSUPPORTED` and proceeds without carrying.
- The snapshot is taken at launch, inside the claim when there is one: `git stash create`, plus untracked non-ignored files for `all`. It never changes the chair's tree or index.
- It becomes the writer's first commit, `wip: carried from chair`.
- The footer lists the carried paths.

| Key | Scope | Default | Values |
|---|---|---|---|
| `lanes.launch.mode` | T/H/P/L | from the template's shape: a leaf is `inline`, a composite or queue is `handle`. A template field `launch_mode` overrides. | `--wait` / `--no-wait` |
| `lanes.read_source` | H/P/R/L | `live`, changed from `auto` | `live|default_branch|auto|<REF>`. `--source` takes the same values, renamed from `upstream|local`. |
| `lanes.destination.write` | H/P/L | `auto` (the table above) | `auto|parent|new` |
| `lanes.worktree.sharing` | H/P/L/O | `warn` (§3.11) | `allow|warn|refuse` |
| `lanes.carry_uncommitted` | H/P/L | `tracked` | `off|tracked|all` |

- **`--isolated`** sets `lanes.access=isolated`, `lanes.sandbox=on`, `policy.default=refuse`, `read_source=<HEAD sha>`, `destination=new` and `worktree.sharing=refuse`. Each of these can still be set on its own.
- **`--detached`** keeps its meaning.
- **`lanes.isolation`** is deleted, as the migration table says.
- **Fan-out** uses a repeated `--brief`: `run task --brief A --brief B` runs one lane per brief in parallel and prints each result under a header. Each child writer resolves its destination independently. The separate `--each` flag is dropped.

What the caller sees from an inline launch:
```
<lane's final answer, bounded at both ends>
— task-3f2 · luna · 2m14s · read-only, live checkout · 0 warnings
  next: provenant output task-3f2 --full
```

- **Past `--timeout`:** the partial tail, then `still running — provenant wait task-3f2`, with exit 124 (§3.5).
- **Detail:** `--json` gives `CommandResult<ResultProjection>`. `--full` adds `LaunchTimings{admission_ms, census_ms, git_ms, worktree_ms, prepare_ms, total_ms}` (#3348).
- **Launch speed:** `domains.run.launch_warn_ms` (10000) only warns. `scripts/bench` gains provider tokens and `--repeats` (#3447).

## 5. Deliver template schema

- **Bundled template:** `crates/fabric-runtime/assets/templates/deliver.toml`.
- **Layers:** host `<config>/templates/deliver.toml`, project `.provenant/templates/deliver.toml`, launch `--template-file`.
- **Lowering:** the existing template compiler lowers the stages onto `loop`, `escalate`, gates, `delivery_review`, hygiene, `over` expansion and park. A new stage kind comes in only when the compiler fixtures need it.

### 5.1 Schema and semantics

```toml
[template]   # name, schema = 1, extends = "deliver"?, entry = "STAGE", doc
[params]     # NAME = { type = "string|integer|boolean|list", default, enum?, doc }; set per item with --set
[routes]     # ROLE = "TIER" (utility|junior|senior|principal) or a host route alias
[ladders.NAME]
rungs = [{ route = "ROLE|TIER", brief = "default|wide", max_rounds = N, min_failures = N? }]
[budgets]    # PHASE = { rounds = N, wall = "DUR"? }
[policy]     # bare names, mapped to P1 keys (table below)
[[stage]]
id = "ID"                      # required, unique
kind = "lane|command|gate|forge|review|fanout"
enabled = true
when = { PARAM = VALUE }       # the existing submission-time equality terms over params
on_skip = "STAGE|land|done"    # required whenever the when or enabled keys can skip the stage
after = "ID" | before = "ID"   # declaration order only, in override layers; edges stay explicit
use = "TEMPLATE"               # lane and review stages
brief = "TEXT"                 # lane: extra brief text, with ${param} substitution
ladder = "NAME" | route = "ROLE"
run = ["argv"] | port = "NAME"; cwd = "DIR"; commit = "MSG"?; protocol = "exit|json"   # command
effect = "publish|conflict|ci|regenerate|land|reconcile"      # gate and forge stages
phase = "PHASE"                # entering the stage charges 1 round of this phase
lease = "ci"
inputs = ["STAGE.OUTPUT", "STAGE.OUTPUT?"]   # ? marks an optional input, empty when skipped
on = { OUTCOME = "STAGE|land|done|park:REASON" }
# fanout only:
over = "item.slices"; child = ["STAGE", ...]; branch = "PATTERN"; base = "PATTERN"; max_parallel = N
merge = { into = "PATTERN", strategy = "merge", order = "deps", on_conflict = "STAGE" }
# review only:
shape = "single|panel"; lenses = [{ lens = "NAME", route = "ROLE|TIER" }]; distinct_families = bool; reconcile_route = "ROLE|TIER"
```

| Rule | Meaning |
|---|---|
| Entry | The `[template] entry`, else the first enabled stage. An adopted item's `entry` overrides both. |
| Skip | A stage skipped by `when` or `enabled = false` routes to its `on_skip` target. The outcomes it would have produced do not exist, and an optional `?` input reads as empty. |
| Ladders | A stage with `ladder` keeps its rung itself. `no_commit`, `uncommitted_edits` and the `repair` outcomes advance the rung and re-run the same stage, continuing the same branch under §3.11. When the last rung is spent, the outcome is `exhausted`. There is no `escalate` stage and no `previous` target. |
| Gates | A gate never routes `pending`. It waits internally with backoff, bounded by its phase `wall`, and emits `timeout` at the bound. `ci` reruns cancelled runs internally. |
| Commands | With `protocol = "exit"` (the default), exit 0 is `ok` and anything else is `failed`. With `commit` set, `ok` splits into `clean` (no diff) and `committed`. With `protocol = "json"`, the stage receives `{item, stage, head, worktree}` on stdin and reads `{"outcome": "..."}` from stdout. `port = "format"` resolves argv and cwd from `config/ports.json`. |
| Layer merge | A stage merges by `id`, key by key. Its `on` table merges by outcome, so a layer that sets `on = {clean = "x"}` keeps the other routes. Lists replace. `extends` applies the parent first. |
| Compile checks | Every declared outcome is routed. Every cycle passes through a stage with a finite `phase` budget. Every path to `land` passes a `ci` gate and an acceptance (a review `pass` or `accept_below`, or `[policy] acceptance = "ci"`), with no head-changing stage between them and `land`. A head change invalidates CI and review evidence for the older head. Merge happens only in `land`, under `domains.pr.land.enabled` (host only); a template cannot set it. Templates name roles or tiers, never models. |

`[policy]` names and the P1 keys they set at the template layer:

| `[policy]` | P1 key |
|---|---|
| `accept_below` | `domains.review.accept_below` |
| `acceptance` | `domains.review.acceptance` (`review|ci`) |
| `generated_patch` | `domains.ci.generated_patch` |
| `conflict_strategy` | `domains.fabric.queue.conflict_strategy` |
| `rerun_cancelled` | `domains.fabric.queue.rerun_cancelled` |
| `head_binding` | `domains.fabric.queue.head_binding.*` |
| `ci_wait` | `domains.fabric.queue.ci_wait.*` |
| `infra_retry` | `domains.fabric.queue.infra_retry.*` |
| `lanes_pr` | `lanes.pr` |

### 5.2 Default template: the fleet's behaviour

```toml
[template]
name = "deliver"
schema = 1
entry = "scope"
doc = "Issue to merged PR: scope, write, format, publish, conflict, CI, review, land."

[params]
plan = { type = "boolean", default = true }
review_shape = { type = "string", default = "single", enum = ["single", "panel", "off"] }
review_template = { type = "string", default = "review", enum = ["review", "review-panel"] }
base = { type = "string", default = "origin/main" }

[routes]
scope = "junior"
reviewer = "senior"
conflict = "senior"
adjudicator = "principal"

[ladders.write]
rungs = [
  { route = "junior",    max_rounds = 3 },
  { route = "senior",    max_rounds = 2 },
  { route = "principal", max_rounds = 2 },
  { route = "principal", brief = "wide", max_rounds = 1, min_failures = 10 },
]

[budgets]
ci = { rounds = 8, wall = "8h" }
review = { rounds = 3 }
review_evidence = { rounds = 2 }
conflict = { rounds = 2 }
prep = { rounds = 2 }

[policy]
accept_below = "high"
acceptance = "review"
rerun_cancelled = true
conflict_strategy = "merge"
generated_patch = "apply"
lanes_pr = "never"
head_binding = { push_lag = "15m", resync = "reopen", resync_after = "5m" }
ci_wait = { wall = "4h", poll_floor = "60s" }
infra_retry = { push = 3, launch = 3, backoff = "20s", factor = 2 }

[[stage]]
id = "scope"
kind = "lane"
use = "plan"
route = "scope"
when = { plan = true }
on_skip = "write"
on = { ok = "write", failed = "write" }

[[stage]]
id = "write"
kind = "lane"
use = "implement.issue"
ladder = "write"
phase = "ci"
inputs = ["scope.result?"]
on = { committed = "format", exhausted = "park:ladder_exhausted", prep_refused = "reconcile" }

[[stage]]
id = "reconcile"
kind = "forge"
effect = "reconcile"
phase = "prep"
on = { ok = "write", failed = "park:operation_error" }

[[stage]]
id = "format"
kind = "command"
port = "format"
commit = "style: format"
on = { clean = "publish", committed = "publish", failed = "park:operation_error" }

[[stage]]
id = "publish"
kind = "forge"
effect = "publish"
lease = "ci"
on = { published = "conflict", held = "park:held", failed = "park:operation_error" }

[[stage]]
id = "conflict"
kind = "gate"
effect = "conflict"
phase = "conflict"
on = { clean = "ci", conflicting = "merge_base", timeout = "park:conflict" }

[[stage]]
id = "merge_base"
kind = "lane"
use = "implement.fix"
route = "conflict"
phase = "conflict"
brief = "Run git merge ${base}; never rebase. Then grep for fields or match arms declared twice."
on = { committed = "format", no_commit = "park:conflict", uncommitted_edits = "park:uncommitted_edits" }

[[stage]]
id = "ci"
kind = "gate"
effect = "ci"
on = { green = "review", red = "repair", generated_only = "regenerate", conflicting = "merge_base", timeout = "park:ci_unavailable" }

[[stage]]
id = "regenerate"
kind = "forge"
effect = "regenerate"
phase = "ci"
on = { applied = "publish", failed = "repair" }

[[stage]]
id = "repair"
kind = "lane"
use = "implement.fix"
ladder = "write"
phase = "ci"
inputs = ["ci.failures"]
on = { committed = "format", flake = "ci", exhausted = "park:ladder_exhausted" }

[[stage]]
id = "review"
kind = "review"
use = "${review_template}"
shape = "${review_shape}"
route = "reviewer"
phase = "review"
when = { review_shape = ["single", "panel"] }
on_skip = "land"        # only valid with acceptance = "ci"; the compiler rejects it otherwise
on = { pass = "land", accept_below = "land", fail = "review_fix", missing_evidence = "review_retry", split = "adjudicate" }

[[stage]]
id = "review_retry"
kind = "review"
use = "${review_template}"
shape = "${review_shape}"
route = "reviewer"
phase = "review_evidence"
on = { pass = "land", accept_below = "land", fail = "review_fix", missing_evidence = "park:review_evidence", split = "adjudicate" }

[[stage]]
id = "adjudicate"
kind = "lane"
use = "review.adjudicate"
route = "adjudicator"
on = { pass = "land", fail = "review_fix" }

[[stage]]
id = "review_fix"
kind = "lane"
use = "implement.fix"
ladder = "write"
phase = "review"
inputs = ["ledger.open_findings"]
on = { committed = "format", exhausted = "park:ladder_exhausted" }

[[stage]]
id = "land"
kind = "forge"
effect = "land"
on = { merged = "done", moved = "conflict", not_authorised = "park:authority" }
```

Two defaults close known gaps in the template:
- `review_shape = "off"` together with `acceptance = "review"` is a compile error that names `review`, its `on_skip` and `domains.review.acceptance`.
- `format` with no `format` port runs `clean` and warns `HYGIENE_NO_FORMATTER`. Provenant's own `config/ports.json` gains `format: {argv: ["cargo", "fmt"], cwd: "tools/front-door"}`.

`deliver.batch`:
```toml
[template]
name = "deliver.batch"
extends = "deliver"
entry = "slice0"

[params]
plan = { default = false }
review_shape = { default = "panel" }
review_template = { default = "review-panel" }

[routes]
reconciler = "principal"     # the owner's host config maps it, e.g. routes.alias.reconciler = "opus"

[[stage]]
id = "slice0"
kind = "lane"
before = "scope"
use = "implement.plan"
route = "principal"
brief = "Land every shared type, key, record, error code and executable seam with fakes and fixtures, compiling and behaviour-preserving."
on = { committed = "slices", no_commit = "park:no_commit" }

[[stage]]
id = "slices"
kind = "fanout"
over = "item.slices"
child = ["write", "format"]
branch = "slice/${item.id}-${slice.id}"
base = "${item.integration_branch}"
max_parallel = 8
merge = { into = "${item.integration_branch}", strategy = "merge", order = "deps", on_conflict = "merge_base" }
on = { merged = "lens_pass", slice_failed = "park:slice_failed" }

[[stage]]
id = "lens_pass"
kind = "review"
use = "review-panel"
shape = "panel"
distinct_families = true
lenses = [
  { lens = "correctness",     route = "senior" },
  { lens = "design",          route = "senior" },
  { lens = "tests",           route = "junior" },
  { lens = "contract",        route = "senior" },
]
reconcile_route = "reconciler"
phase = "review"
on = { pass = "publish", accept_below = "publish", fail = "review_fix", missing_evidence = "review_retry", split = "adjudicate" }

[[stage]]
id = "review"
when = { review_shape = "done" }
on_skip = "land"

[[stage]]
id = "scope"
enabled = false
on_skip = "write"
```

The `lens_pass` stage lists `correctness`, `design`, `tests` and `contract`, which are lenses that already exist (`review.<lens>.toml`). Inside `slices`, the `write` and `format` child stages target the slice branch. A child's `exhausted` outcome becomes `slice_failed`. After `lens_pass`, the inherited path runs `publish` on the integration branch, then `conflict`, `ci`, `review` and `land`. `review` is skipped by its overridden `when`, and `lens_pass` counts as the acceptance.

### 5.3 Customised example: a docs project

```toml
# .provenant/templates/deliver.toml
[template]
extends = "deliver"

[params]
plan = { default = false }

[policy]
accept_below = "medium"
conflict_strategy = "rebase"

[[stage]]
id = "format"
run = ["npx", "prettier", "--write", "docs"]
cwd = "."
commit = "style: prettier"
on = { clean = "linkcheck", committed = "linkcheck" }

[[stage]]
id = "linkcheck"
kind = "command"
after = "format"
run = ["lychee", "--offline", "docs"]
on = { ok = "publish", failed = "repair" }
```

The bundled template, `deliver.batch` and this example are compiler fixtures from batch 2 slice 0 onward.

## 6. What is deleted

| Deleted | Replaced by |
|---|---|
| Fleet: `~/.local/state/chair-driver/fleet/{fleet.mjs,batch.mjs,hooks.mjs,fleet.json,state.json,inbox.jsonl,needs-chair.md}` | Moved to `~/.local/state/chair-driver/archive/fleet-<date>/` after batch 2's on-host checks. Never deleted. |
| `graph_pass_before_forge`, the serial task vector, the `notice_batch_flush`/`quota_refresh` placeholders, the "graph before forge" contract (`cli.md:127`) | `TickTask` registry, `Deadline` |
| `advance_after_remote_terminal` | `progress::wake` |
| The selection in `install_report.rs` | `graph::root_cause` |
| The config-layer source enums listed in §3.3, `QUEUE_KNOB_PRECEDENCE`, `seal_queue_knob(s)` | `provenance::Layer`, `seal_settings` |
| `BoundaryClass::{LaneGit, Integrity}` (`diagnostic.rs:22`); the dead `DIRTY_WRITER` rule; `VerificationResolution.conflict` | `policy::Boundary`, warnings |
| `check_writer_lineage`'s ownership refusals, `RunError::WriterLineage`, `WriterOwnership`, `Occupancy::Held → WriterBusy` as an unconditional refusal, "nested provisioning belongs to another lane" | `WorktreeLease`, `admit_writer`, `guard_destructive`, `lanes.worktree.sharing` |
| `CommandError.repair`, `Warning.repair` | `next: Vec<NextStep>` |
| `lanes.isolation`; `--source upstream|local`; `upgrade --hosts`; `upgrade --host` (smoke placement) | `lanes.read_source` and `lanes.destination.write`; `--host`; `--verify-host` |
| `stop/retry --queue-control/--queue-revision/--operation-id` | `provenant queue …` |
| `delivery_execution_wired`, the `TODO(#3972-s3)` nodes, the `deliver`, `batch` and `plan-then-deliver` composites, `accept_inconclusive_without_blocking` | `deliver.toml`, `deliver.batch`, `domains.review.accept_below` |
| Graph rewriting in `queue_delivery.rs`, including `bind_item` (its caller in `src/fabric/interface.rs:2551` moves to the stage compiler) | `StageEffect` |
| Direct CI reads outside `pr/observer.rs` | `observe_head` |
| The swift-fixtures CI step and `scripts/check-swift-fixtures` | `scripts/check-generated` and the `generated` job; `generated-reference` stays |
| `gc_result` exit 3 on a budget stop; `NodeProgress.warnings: Vec<String>` | partial exit 0; `Vec<Warning>` |
| Docs: "No new public verb is required" (`delivery-queue.md`); "one writer per worktree" (`skills/orchestrate`, `lane-contracts.md`); the hand repair procedure in `skills/deliver` | this spec, folded into `delivery-queue.md`, `cli.md`, `lane-contracts.md` and `run-launch.md` |
