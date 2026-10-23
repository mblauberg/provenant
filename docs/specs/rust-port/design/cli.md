# Command surface

Status: **proposal for the command-tree freeze**, 2026-10-24. Covers
`provenant-cli` and the `provenant` binary. The chair freezes this tree after
reconciling the other area designs; a reserved subtree below is not an accepted
feature list. Author route: `codex/gpt-6.1-sol@high`.

## Purpose

The command surface gives people, skills, harness hooks and CI one executable
for starting work, inspecting its durable result and applying explicit policy
checks. `cli` parses arguments and renders typed results; the binary composes
library owners and selects the transport. Neither owns provider execution,
process custody, configuration storage or Fabric policy. The design follows
[PORTING.md v7](https://github.com/mblauberg/provenant/blob/1b3d48e4c549811ce3636edc30b047f36fd24ada/docs/specs/rust-port/PORTING.md), especially R0, R1 and R9, and the dependency
allowlist in [ADR 0028](../../../adr/0028-rust-port-dependencies.md).

## Evidence and pruning

Source baseline: `1b3d48e4c549811ce3636edc30b047f36fd24ada`. Inspected the
front-door router, help, launch/resolution code and process tests; every module
in `crates/cli`; the harness/release/security/date scripts and their tests;
installer registrations, skill callers and CI. There is no top-level `hooks/`
in this checkout: callbacks live in `scripts/hooks/`, Python hook scripts and
`skills/session/scripts/`.

Transcript snapshot (call timestamps 2026-10-13 to 2026-10-23 UTC):
1,401 Claude project JSONL files and 1,046 Codex session JSONL files; 79,766
Claude Bash calls and 37,431 literal `tools.exec_command` inputs inside Codex
`exec` records. Parse only tool-call records, decode shell strings, remove
heredoc bodies, split shell command positions and unwrap shell/env launchers.
Exclude prose, file edits, quoted source and `which`/`command -v`. Count an
attempt, not success: output/status was not paired. Dynamic argv, subprocesses
inside scripts and harness-triggered callbacks are outside this count; two
verbs were dynamic. No legacy direct Codex shell function calls were found.
Active transcript files can grow; these numbers are one snapshot, not a
project-wide lifetime census. No credentials were opened or copied.

| Old root verb | Attempts | Proposed disposition |
| --- | ---: | --- |
| `help`, `root` | 153, 48 | Keep cheap discovery/identity. |
| `fabric`, `lanes` | 199, 619 | Keep one Fabric subtree; move root `lanes` into it. |
| `route`, `routes`, `quota`, `profile` | 8, 12, 5, 1 | Route listing/resolution includes quota and resolved controls; drop separate profile mutation and quota scheduling/credit commands. |
| `telemetry`, `stats`, `retro` | 8, 5, 0 | One telemetry subtree; drop root report aliases and unused retro/change-marker surface. |
| `hosts`, `peer`, `lease` | 8, 1, 1 | Fabric owns host/peer transport and landing operations; no root synonyms. Lease use was `status`. |
| `dispatch`, `clean` | 4, 3 | Keep (dispatch includes two help calls; clean includes one). |
| `check`, `pr` | 1, 1 | Keep content checks for CI; `pr` was help only, so remove the dedicated polling wrapper. |
| `worktree`, `lint`, `capabilities` | 0, 0, 0 | Direct owners: worktree 56 (4 help); lint and capabilities one help each. Keep worktrees; internalise capabilities and lint policy. |
| `digest`, `classify`, `extract`, `reap`, `gate`, `batch`, `run`, `lane`, `events`, `graph` | 0 each | Drop these root commands. No direct utility, reap or batch/run-controls script calls observed. Batch/graph/events need new owner evidence before any reservation becomes a feature. |

Other attempts: unsupported `doctor` 1, no-verb `--version` 2, dynamic verb 2;
total **1,082**. Nested Fabric calls include dispatch 58 (5 help), inbox 41,
send 46 (5 help), whoami 22, status 3, landing-push 3 (1 help), and one each
of adapters/note/peers/session/session-list/tasks. Fabric batch/graph/events:
zero. Direct policy scripts: static-security 89, timebomb 52, check-harness 1
(help only), release wrapper 1 (`--publication-range`); other harness/release
entry points and `gate`: zero. Direct safety guard 2, session hook install 2,
compact reinjection 1; no routing/output hook command. Automatic callbacks
are not visible in shell-call counts.

Complete observed CLI flag ledger (attempts, across all verbs):

| Destination / decision | Old flag counts |
| --- | --- |
| Shared output/discovery | `--help` 32; `--json` 19; `--version` 2. Keep. |
| Fabric listing/waits | `--all` 192; `--summary` 12; `--wait` 493; `--timeout` 491; `--detail` 1. Owner proposes the minimal canonical grammar. Drop unsupported `--any` 2 and `--ids` 1. |
| Dispatch/routing | `--access-mode` 2; `--adapter` 9; `--alias` 4; `--confidential` 1; `--effort` 51; `--id` 51; `--mode` 51; `--model` 55; `--operation-id` 51; `--prompt-file` 53; `--role` 7; `--task-class` 2; `--task-id` 3; `--tasks` 2; `--worktree` 50. Owner must collapse redundant IDs/modes; observation alone does not freeze an option. |
| Fabric coordination | `--digest` 6; `--kind` 21; `--label` 2; `--limit` 35; `--peek` 36; `--reply-to` 3. Reserved to Fabric's proposal. |
| Cleanup | `--merged-since` 2; `--prune-merged` 2. Preserve explicit merged-worktree cleanup through supervisor policy. |
| Remove/replace | `--cwd` 3 becomes global project selection; `--resolved` 1 becomes resolved route data. Drop caller-selected `--run-dir` 2 and `--output-path` 1: owners choose custody-safe artifact paths. |

Direct worktree flags: `--new-branch`/`--start-point` 42 each,
`--existing-branch` 2, `--help` 4. Other direct-owner flags:
`--candidates-file`/`--spread-state-file`/`--role`/`--task-class` one each in
model routing; internalise snapshot/spread plumbing. Release
`--publication-range` 1 becomes `check release --base OID --head OID`.
Unobserved front-door help flags (zero): `--account`, `--add`, `--apply`,
`--at`, `--branch`, `--compare`, `--expected-sha`, `--file`, `--follow`,
`--grace`, `--health`, `--ignore-backoff`, `--labels`, `--node`,
`--non-confidential`, `--plan`, `--power`, `--project`, `--refresh`,
`--remote`, `--risk-tier`, `--schema`, `--seconds`, `--since`,
`--started-before`, `--sync`, `--tail`, `--tree`, `--trigger`, `--window`.
Do not carry old unobserved flags forward by default. New safety/isolation
controls below have a stated requirement, not an invented usage count;
other owners must justify exceptions at freeze.

Caller search found 393 command mentions in 114 files, including examples;
that is an inventory, not execution evidence. Required callers include CI's
`check-harness --checks-only`, gate's release wrapper, orchestrate's lane
waits, `docs/worktrees.md` helpers and installer hook registrations. Keep
content/publication checks for those callers even without interactive use.
Prune the remaining implementation weight as follows:

| Removal | Reason / replacement |
| --- | --- |
| Every root alias listed above; short/alternate flag spellings | One canonical owner path. Routing tier names are domain values, not command aliases. |
| Public utility/schema/Ajv surface | Zero direct CLI use; drop the public commands. Internal classifier dependency below must be resolved before deleting the engine. Extract is already removed by the rewrite plan. |
| Hidden `lane run`; no-argument MCP selected by environment | Supervisor owns its runner. MCP is explicit `fabric mcp`; no arguments always means usage. |
| `check --doctor`, `--checks-only`; composite `gate` | Separate product checks from Cargo/CI orchestration. No pytest/npm, provider probes or home-workflow scan inside `check`. |
| Stale-checkout advice on every command | Remove implicit Git subprocesses; expose installation identity through root/checks. |
| Output-digest and roster-advice hooks | Optional registration/docs without demonstrated automated value; they add provider execution/output rewriting or repeat route data. |
| Post-merge cleanup hook | Move implicit deletion into the explicit merge/worktree workflow under its existing authority. |
| npm retry, Python AST tooling, old oracles/emulation | Their subjects disappear. R7 guards and new-design tests replace them. |

One indirect caller prevents calling the utility engine unused:
`task_classifier._run_model()` invokes `utility("extract", …)`, reached by
`model_route_cli` and the default/shadow `model` chains in
`config/model-routing.json`. These internal calls are outside the transcript
command count. Propose explicit/rules/default classification only, removing
implicit model/shadow provider calls. The execution-route owner must accept
that change and remove the corresponding config/tests/callers, or supply a
typed internal classifier replacement, **before** utility/schema deletion.
This is a freeze dependency, not a Python compatibility bridge.

## Proposed tree and conventions

All names below have one spelling. There are no hidden aliases, abbreviated
long flags, positional command rewrites or short flag synonyms, including
`-h`. Every leaf has `--help`; `help [COMMAND…]` follows the same Clap command
model. `help --json` emits the command catalogue, including options and value
constraints. Help never resolves roots, opens SQLite, starts threads, probes
providers or invokes a command. The dependency allowlist already includes
Clap derive; do not add completion, colour or shell-parser dependencies.

```text
provenant
  help [COMMAND…]
  root
  install                         [state.md reservation]
  route                           [execution-route.md reservation]
    list | resolve
  dispatch                        [execution-run.md reservation]
  fabric                          [fabric.md reservation]
    mcp | …                       (messages/tasks/claims/lanes/hosts/lease)
  telemetry                       [telemetry.md reservation]
    ingest | report
  worktree                        [supervisor.md reservation]
    create | list | remove
  clean                           [supervisor.md reservation]
  workflow …                      [workflow.md reservation]
  check
    harness
    release [--base OID --head OID]
    dates
  hook
    claude safety
    claude pre-compact            [workflow.md policy reservation]
    claude compact                [workflow.md policy reservation]
    git pre-push | commit-msg FILE
```

`dispatch` is the single execution entry; remove `fabric dispatch` as a CLI
forwarding synonym, while Fabric MCP may retain its typed execution tool.
The ellipses reserve namespace ownership, not permission to ship every old
verb. Area designs supply leaf grammar, flags, bounds and usage evidence;
the chair reconciles them here before any parser lands. A disagreement about
placement returns to the chair, not to an alias. MCP tools are owned and
registered by `fabric-mcp`; CLI adds **no MCP tools**. CLI and MCP call the
same typed library operations, never each other's text/JSON parser. The binary
calls `fabric-mcp` directly for `fabric mcp`.

Global options, accepted before or after a subcommand:

| Option | Meaning |
| --- | --- |
| `--help` | Pure help for the selected command, exit 0. |
| `--version` | Package version and embedded workspace source digest, exit 0. New installation identity requirement; no root lookup. |
| `--json` | One versioned result on stdout, including errors. Invalid for `hook` or `fabric mcp` execution; reject before reading stdin. Pure help remains available. |
| `--state-root PATH` | Absolute fresh-state root; overrides `PROVENANT_STATE_ROOT`, then the state owner's default. New requirement for isolated tests and installations. |
| `--project PATH` | Explicit project selection instead of the caller's cwd. Canonicalised and verified by the state/Git owners; does not change the process cwd or select a product checkout. |

No `--verbose`, `--quiet`, `--color`, arbitrary owner argv forwarding, root
component overrides, legacy root environment fallbacks or machine-global
provider settings switches. Unknown flags, duplicate singleton options and
conflicting options are errors. Repeatable selectors must be declared as such
by their owner. Durations use bounded non-negative integer seconds, counts
are bounded integers, and IDs/revisions are newtypes. Paths remain `OsString`
until a record needs text. Leaf help states input size and wait bounds.

`root` reports product asset root, fresh state root, selected project and
installation version/digest. Human output is labelled lines; JSON is a typed
roots record. It is read-only and cheap: no threads, database or probes. It
never searches the old roots for an installation. Installation selection,
root precedence and managed files belong to `config`, not a second resolver
in `cli`.

### Results and failures

Human results go to stdout; failures use `provenant: CODE: message` on stderr,
with a bounded cause and repair action when one exists. No traceback, ANSI
sequence, prompt text, credentials or matched secret bytes. Findings name the
path/object and rule, not the matching content. Internal diagnostics use R8's
separate bounded stderr writer; logging failure cannot change command state.

JSON is UTF-8, one object plus LF, never a progress stream:

```json
{"schema_version":1,"command":"fabric.lanes","status":"ok","data":{},"error":null}
```

`status` is `ok`, `failed`, `refused`, `cancelled`, `timeout` or
`error`. Non-success results may retain useful typed `data`; `error`, when
present, contains a stable code, redacted message, cause and optional repair.
No library returns pre-rendered stderr or chooses an exit code. Parse errors
also use this envelope when an exact `--json` token occurs before `--`;
pre-scan only that transport selection, then let Clap validate all arguments.
Help's JSON data is the catalogue; version's is the build identity.
A running/pending job is query data under `ok`; only a requested wait can
expire with 124.

| Exit | Meaning |
| --- | --- |
| 0 | Successful completed operation; informational query may describe a running job. |
| 1 | Operation failed, policy refused or check findings exist. |
| 2 | Invalid command/input/configuration version or incompatible transport flags. |
| 3 | Operation cannot proceed: setup/I/O failure, contention, unavailable required evidence. Never success. |
| 124 | Explicit wait deadline expired. Does not cancel the underlying run. |
| 130 | This invocation was cancelled; owner acknowledgement precedes custody release. |

A provider's exit is result data, not the CLI's arbitrary process exit. SIGPIPE
on a human read-only listing is success; failure writing a mutating command's
result is an I/O failure after recording its outcome, never a second execution.
A JSON write failure is 3; it cannot promise a complete envelope. Blocking
waits have explicit deadlines and stop polling on cancellation. Streaming is
restricted to externally defined MCP/hook channels; future follow operations
need a separate, explicitly versioned transport proposal.

Results are at most 1 MiB; paginated list records carry `next_cursor`, and
result tails are capped at 500 lines/64 KiB. Do not silently discard rows.
`fabric lanes` must inspect and cancel direct dispatches as well as Fabric
dispatches through one run registry; no second CLI status store.
Non-UTF-8 paths use a records-owned tagged value (`utf8` string or `unix_bytes`
lowercase hex); human rendering escapes raw bytes. Do not reproduce Python's
surrogateescape, repr, whitespace, float or argparse behaviour.

## Hook and check boundaries

Hook processes bypass ordinary result envelopes and logging on stdout. Read
at most 1 MiB stdin, accept additive harness fields, validate the fields the
handler uses, and never persist the input. Existing registrations have a
30-second host timeout for safety; handlers use a one-second local policy
budget and never launch providers. A safety hook is an additional guard;
confinement and process custody remain enforced by their library owners.

| Handler | Input / output / exit and owner |
| --- | --- |
| `hook claude safety` | Claude `PreToolUse`, matcher `Bash`; JSON `tool_name`, `tool_input.command`. Allow: empty stdout, 0. Deny: one JSON line with `hookSpecificOutput.hookEventName="PreToolUse"`, `permissionDecision="deny"`, `permissionDecisionReason`, exit 0. Non-Bash/malformed envelopes produce no decision and bounded diagnostics; preserve the current fail-open transport behaviour rather than claiming a malformed event was checked. `confinement` owns shell-command refusal policy. |
| `hook claude pre-compact` | Claude `PreCompact`, JSON `cwd/session_id`, existing five-second host timeout. Workflow owns session-state validation; emit bounded advisory text only, always exit 0. No provider or transcript scan. |
| `hook claude compact` | Claude `SessionStart`, matcher `compact`. Workflow provides the current session checkpoint from the **new** root; emit at most 8 KiB context, otherwise nothing, always 0. Drop `transcript_path` rummaging: R1 prohibits reading old transcripts at runtime. Workflow must define checkpoint publication before this reservation can ship. |
| `hook git commit-msg FILE` | Git supplies the message-file argument, no stdin contract. Workflow validates the bounded file through safe FS; stdout empty, 0 permits, stderr findings and 1 refuses. Never rewrite the commit message. |
| `hook git pre-push` | Git argv remote name/location plus stdin records `local-ref local-oid remote-ref remote-oid`. No stdout; stderr reason and 1 on refusal, otherwise 0. Validate Git's native 40/64-digit object IDs and all-zero deletion IDs. Keep default-branch deletion/non-FF/linked-worktree push and conflict-marker guards; inspection failure refuses. Supervisor/confinement owns verified Git evidence. |

Only Claude hook protocols are registered in today's installer. Codex,
Cursor, OpenCode, Kiro and agy installation paths do not thereby acquire a
Claude hook contract; register no invented equivalent. State/config owns
`~/.claude/settings.json` (or `CLAUDE_CONFIG_DIR/settings.json`) managed-entry
updates, preserving unrelated entries. Other harness config locations and
MCP registrations stay with state. Git hook files are tiny managed `exec`
launchers to this binary, quoted with absolute paths; they replace no foreign
hook. No Python interpreter or copied Rust source goes into a hook.

`check harness` inspects an explicit verified project checkout: external
`SKILL.md` frontmatter, OpenAI sidecars and local asset references; skill route
fixtures in their new JSON format; local Markdown links/anchors and GitHub
issue forms. It returns ordered `Finding { rule, path, line, message }` rows
and a count. `records` supplies R6's external YAML subset reader. Traverse
tracked inputs deterministically, refuse unreadable inputs, and never follow
symlinks out of the selected checkout. Shell syntax checks, any remaining
JS/Python checks and test runners belong in CI while their subjects remain.

`check release` scans tracked files and raw HEAD for required legal/public
files, forbidden private paths, symlinks, size violations (5 MiB per blob),
secrets and personal home paths. With both `--base` and `--head`, scan raw
objects for the nonempty ancestor range and selected head tree, including
added-then-deleted blobs, paths and complete author/committer/message data.
This retains the observed publication-range use. Drop `--history`/all-ref
traversal (zero invocations); the result names the exact attested range/tree.
No publishing, fetching or rewriting. Use trusted Git and sanitised evidence
reads: no inherited repository/object/config redirects, replacements, grafts,
fsmonitor, textconv, external diff or lazy promisor fetch. Loose/packed objects,
linked worktrees and native alternates work. Missing/truncated objects,
unreadable/bounded-out files and non-UTF-8 content never become clean results.
Scan bytes with one rule registry; any prefilter must prove it is a superset.

`check dates` scans tracked Rust test sources for fixed ISO-date literals used
with a real-current-time expression. Reuse only the small textual engine's
idea; remove JS syntax matching and Python AST inference. Require an injected
clock in time-dependent tests, or a local `timebomb: allow REASON` comment for
a deliberate fixed date. No inferred cross-file Python bindings, old-baseline
diff filter or dependency on today's date. Unit tests document conservative
textual matching; it is a lint, not proof of time independence.

## Records and state

CLI has no private database, cache, run registry, approval file or command
journal. Proposed default root is
`${XDG_STATE_HOME:-$HOME/.local/state}/provenant/rust-v1`; `state.md` owns its
final spelling and must reconcile it before freeze. `--state-root` and
`PROVENANT_STATE_ROOT` have identical semantics across CLI, MCP and hooks.
Runtime code never reads, migrates or deletes `~/.agents`, `.agent-run/`, old
telemetry stores or transcripts. Only the explicitly scoped Fabric/telemetry
imports permitted by R1 may read their old message/task/ledger inputs.

| Record / layout relative to new root | Owner and write rule |
| --- | --- |
| `installation.json`, `config.json` | Config's versioned installation identity and config. Atomic replace, file fsync and parent-directory fsync through supervisor safe FS. Root/help never create them. |
| `projects/<project-id>/…` | State-owned project identity; run/Fabric/telemetry layouts are specified by those designs. CLI receives typed roots and IDs, never invents a second path convention. |
| `projects/<id>/sessions/<session-id>/checkpoint.json` | Workflow proposal: version, project/session ID, source checkpoint ID, bounded context and publication timestamp. Atomic publish before compact callback can read it. No transcript path or prompt body. |
| `CommandResult<T>` / `Finding` / `HelpCatalog` | Records-owned version-1 transport models, ephemeral by default. Declared fields; reject unsupported major versions. Additive output fields are allowed within v1; consumers ignore unknown fields. Strict owned input records reject unknown fields unless their owner specifies otherwise. |

An explicitly requested report file, if later justified by a caller, uses the
same atomic publisher; there is no generic output-file flag in this proposal.
Run cancellation, claims, checkpoints and terminal results use their owning
library's atomic/transaction protocol. Printing a success happens after the
owner has committed. An ENOSPC, broken pipe or diagnostic failure cannot be
turned into a provider stop request.

## Crate seams

`cli::parse(args: impl IntoIterator<Item=OsString>) -> ParseOutcome` returns a
typed `Invocation`, pure help/version request or structured usage error.
`Invocation` contains `Globals` and a closed `Command` enum; it has no loose
JSON argv blob. `cli::render(result: &CommandResult<T>, mode, writer)` and
`cli::exit_status(&Outcome)` are edge functions used by the binary only.
No library imports `cli` (R2). There is no `cli` service API for other areas:
they expose request/result/error types, and the binary's exhaustive match
connects each parsed request to its owner. Hook codecs live at this edge;
their policy types live with confinement/workflow.

| Needed API | Supplying area |
| --- | --- |
| Typed roots, project selection, install identity/asset resolution, managed hook/MCP registrations | core/records/config (`state.md`) |
| Bounded safe process runner and FS publisher, signals/cancellation handles, trusted Git object reads, verified worktree operations | supervisor/confinement (`supervisor.md`); cli never uses `Command::new`, raw descriptors or `unsafe` |
| Dispatch/start/status/cancel requests and durable outcomes; route listing/resolution | run/routing/providers/quota (`execution-run.md`, `execution-route.md`) |
| Ingest/report records and explicit ledger import | telemetry (`telemetry.md`) |
| Fabric operations and MCP transport lifecycle, bounded cancellation with custody retained | fabric-runtime/fabric-mcp (`fabric.md`) |
| Branch/commit/review/check policies, GitHub check evidence, session checkpoints | workflow (`workflow.md`) |

Move existing `cli::{clean,worktree,git_evidence,lane_safety}` policy to
those owners, leaving argument/output code here. `install` moves to config;
`reap` and `pr_checks` retire with their unused commands. Retire `projection`,
`retry_install`, `pending`,
Unicode tables, `oracle`, and stale-checkout policy. Reuse useful typed policy
and byte scanners after removing Python semantics and oracle tests. The binary
has `#![forbid(unsafe_code)]`, an exhaustive command match, R8 diagnostics and
release `panic="abort"`; supervisor alone owns syscall wrappers and custody.
No async runtime or daemon is introduced.

## Contract fixtures and shared test harness

External contracts are provider CLI argv/environment/streams/results; harness
hook stdin/stdout/exit/configuration; `SKILL.md` YAML; MCP JSON-RPC 2.0 over
newline-delimited stdio; and Git. Our old help, argparse text, JSON shapes and
paths are not external contracts. Current hook tests construct synthetic
payloads; they do **not** constitute recorded external protocol evidence.

Create `tools/front-door/tests/fixtures/external/{providers,hooks,mcp,git}/`.
Each recording has a manifest with tool/version, capture date, direction,
redactions and byte digest. Keep argv boundaries, non-secret environment keys,
stream bytes and terminal status; never record credential values. Existing
`tests/fixtures/fabric-v1/provider-*.json` and `fabric-context/*` are candidates
for inspection, not automatically accepted recordings. Before enabling each
provider/hook/MCP registration, its owner supplies a sanitised real recording
and a fixture test; no live provider run is authorised by this design lane.
Git tests create real repositories with the trusted Git executable and record
NUL-delimited porcelain/object bytes, including SHA-256 where supported.
YAML tests include every tracked external frontmatter/sidecar/issue form with
`include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/…"))`.

The binary's `tests/support/` is the shared **test** API for all areas:

- `TestRig` creates a `tempfile::TempDir`, empty HOME/XDG/config/cache/state
  roots, a disposable project Git repository and explicit fake-provider
  registrations. Clear ambient provider/Fabric/root variables; use only
  fixture config and `--state-root`. Put sentinel old roots beside it and
  assert byte-for-byte unchanged after each scenario. Canonicalise temp paths
  on macOS (`/var` and `/private/var`). No test resolves a real installed CLI,
  credentials, network endpoint or user configuration.
- `FakeProvider` is a Rust **test target**, `harness=false`, never an installed
  product binary or a new workspace library. Build it before running process
  tests with `cargo test --no-run --message-format=json`; the test launcher
  filters compiler-artifact records by package and `provider_fixture` test
  target, then passes its exact executable path to `TestRig` (not a PATH search
  or a guessed `CARGO_BIN_EXE_*` for a test target). Its own `main` exits 0 when
  Cargo invokes it without a scenario; rig launches select an explicit fixture
  scenario. The test launcher owns this prebuild step.
  The only shipped executable remains `provenant`. Scenario plans control
  recorded wire emission, malformed/truncated output, output flood, exit,
  waiting for input, silence and descendants. It records only fixture argv
  and permitted environment keys. All child creation uses supervisor's safe
  runner, including test support; no `Command::new` exception outside it.
- `TestRig::command` invokes `env!("CARGO_BIN_EXE_provenant")` through that
  runner and captures bounded stdout/stderr/status separately.
  `McpClient` starts `provenant fabric mcp`, performs initialize/initialized,
  lists/calls tools, correlates IDs, sends cancellation and drives EOF. It
  asserts stdout contains only JSON-RPC frames and drains stderr separately.
  Bounded reader threads cannot deadlock on output floods.
- `Barrier`/FIFO handshakes publish ready/spawn/terminal/cleanup events. Tests
  advance only on those events, with watchdog deadlines for a broken fixture;
  no sleep establishes ordering. Explicit teardown cancels only owned test
  processes, waits for terminal publication, then reaps and removes temporary
  files. Abort-profile cleanup cannot rely on `Drop`. A separate parent rig
  retains custody while a test intentionally crashes its command child.

Per R9, unit tests beside each module prove parse bounds, conflicts, refusal,
result/error/exit mapping, byte paths, YAML bounds, release rule matching and
clock injection. Binary integration tests exercise **every accepted leaf**,
help catalogue completeness, human/JSON errors, no-root help, cheap root,
isolated install/hook registrations and all retained external fixtures.
Process tests on Linux and macOS prove: silence and logging/ENOSPC failures
never kill a healthy provider; one writer per worktree; cancellation affects
only its target; controller/MCP EOF does not abandon custody; a delayed reader
cannot block terminal publication; PID reuse cannot target another process;
publication survives controller exit, then cleanup reaps last. Re-express
these invariants before deleting `tests/lane_conformance/`; never compare
against the old implementation.

CI runs Linux fmt/Clippy/workspace tests, Linux and macOS process contracts,
and a clean macOS release build with no Python/Node installed or invoked by
product tests. R7 bans unsafe outside supervisor::sys and direct process
creation outside supervisor; negative compile/lint fixtures prove both guards
actually reject violations. Keep Python security/date checks and remaining
Python/TS jobs until their target sets are empty, then remove their setup,
oracle environment, npm jobs and package smoke. Rust integration/MCP/install
smokes replace the covered behaviour before old tests disappear. Keep workflow
security lint, exact-head checks and the fail-closed `ci-status` aggregate;
update its needs to the new job graph, never bypass it. Local build work runs
only fmt, Clippy and touched-crate/target tests; full gates run in Actions.

## Deletion accounting

Counts below are physical lines (including comments/blanks) at the source
baseline, not promises to retain old test assertions. The build deletes each
subject's Python/TS tests/helpers in the same PR that lands its Rust owner.
Mixed-owner files are claimed once at the final owner removal; earlier owners
delete their sections without leaving adapters. No TypeScript is exclusively
owned by this area; Fabric's TS tree and MCP smoke belong to `fabric.md`.

CLI-owned deletion set (security/date/helper rows wait for their target sets
to empty):

| File | Lines |
| --- | ---: |
| `scripts/check-harness` | 259 |
| `scripts/check_harness.py` | 365 |
| `scripts/check_target.py` | 164 |
| `scripts/gate` | 54 |
| `scripts/public-release-check` | 6 |
| `scripts/public_release_check.py` | 571 |
| `scripts/static-security-check.py` | 530 |
| `scripts/lint-timebomb-fixtures.py` | 311 |
| `scripts/lane_safety_guard.py` | 456 |
| `scripts/utility_output_hook.py` | 129 |
| `scripts/routing_context_hook.py` | 85 |
| `scripts/configure-claude-hooks.py` | 134 |
| `scripts/retry-npm-install.py` | 86 |
| `scripts/check-skill-javascript` | 22 |
| `scripts/lib/harness-python.sh` | 77 |
| `scripts/lib/stale_checkout.py` | 76 |
| `scripts/lib/projection.py` | 70 |
| `tests/test_projection.py` | 162 |
| `scripts/hooks/post-merge` | 38 |
| `scripts/hooks/pre-push` | 55 |
| `tests/test_harness_contract.py` | 484 |
| `tests/test_harness_document_contract.py` | 123 |
| `tests/test_public_release_check.py` | 804 |
| `tests/test_static_security_check.py` | 253 |
| `tests/test_timebomb_fixture_lint.py` | 281 |
| `tests/test_lane_safety.py` | 515 |
| `tests/test_utility_hook.py` | 149 |
| `tests/test_retry_npm_install.py` | 80 |
| `tests/test_stale.py` | 204 |
| `tests/support/front_door.py` | 22 |
| `tools/front-door/crates/cli/src/pending/generate_unicode.py` | 24 |

Shared deletions: these files disappear in the named owner's build slice;
CLI must update its callers/tests there. Counts are not added to this lane's
net estimate. Shell launchers `scripts/worktree` (7) and `scripts/provenant-pr`
(5) retire with their Python owners.

| Owner | Files and lines |
| --- | --- |
| state | `scripts/check-provenant-install.py` (165); `scripts/lib/front_door_build.py` (249); `scripts/install-provenant-command.py` (272); `scripts/lib/product_root_resolver.py` (107); `tests/test_check_provenant_install.py` (358); `tests/test_front_door_build.py` (342); `tests/test_install_provenant_command.py` (272); `scripts/install-harness` (702); `tests/test_install_harness.py` (2,427) |
| supervisor | `scripts/clean.py` (953); `scripts/worktree.py` (982); `scripts/reap.py` (258); `tests/test_clean.py` (1,526); `tests/test_worktree_policy.py` (1,018); `tests/test_run_artifact_cleanup.py` (199) |
| workflow | `scripts/git_evidence.py` (341); `tests/test_git_evidence.py` (346); `scripts/provenant_lint.py` (207); `tests/test_provenant_lint.py` (231); `scripts/provenant_pr.py` (223); `tests/test_provenant_pr.py` (186); `skills/session/scripts/state_check.py` (282); `skills/session/scripts/compact_reinject.py` (169); `tests/test_session_state_check.py` (509); `tests/test_ci_repository_assurance_policy.py` (772) |
| execution-route/run (classifier decision above) | `scripts/utility.py` (241); `tests/test_utility.py` (154); JS helper `scripts/utility-schema.cjs` (16); `scripts/task_classifier.py` (190); `tests/test_task_classifier.py` (107) |
| final CI | `tests/test_rust_port_layering.py` (52) |

`tests/test_lane_safety.py` mixes guard, reap, Git and installer subjects;
coordinate those removals in the owner slices. The session pair and checkpoint
policy require workflow agreement. The setup-repo Claude tracker template is
owned by that skill, not one of these managed runtime hooks.

Also delete CLI Rust scaffolding: `pending.rs` (394),
`pending/python_unicode.rs` (782), `oracle.rs` (142), `projection.rs` (451),
`retry_install.rs` (270), `stale_checkout.rs` (401), `pr_checks.rs` (290)
and `reap.rs` (259): **2,989 Rust lines**.
Replace binary router/exec/check/resolve/stale/manual-JSON/help modules and
`tests/front_door.rs`/`tests/common/mod.rs`; transfer `src/lane/` custody to
supervisor before removing its old binary modules and process tests. These
moves provide no double-counted reduction. Golden CLI/MCP judges and
`tests/lane_conformance/` retire with their owning subjects as required by the
rewrite plan; their directory totals belong to those owner inventories.

The exclusive table totals **6,589 lines**, including **6,078 Python**, zero
TS and 511 shell lines. Budget 3,000–4,000 net additional Rust lines for parser,
checks, hook edges and shared tests, excluding code merely moved between
crates. Against Python plus the 2,989 deleted Rust scaffold lines, expected
`Net lines:` is **−6,067 to −5,067** across Python/TS/Rust, plus 511 shell
lines removed. Further router/oracle/test simplification is uncredited upside.
Recount the actual diff in each PR; budget growth needs an explanation.

## Build order

1. **Parser and rig.** Land the frozen Clap model, result/error/exit renderer,
   typed binary composition and shared isolated rig/fake-provider/MCP client.
   Delete the shell-owner router, hand-written JSON codec, duplicated help
   files and Python launcher path; do not keep forwarding routes for owners
   not yet built. Remove CLI pending/oracle/projection scaffolding. Root/help
   tests replace front-door delegation tests. No install in this lane.
2. **Checks.** With records/config and safe Git/FS available, land
   `check harness`, release artifact scanning and Rust date lint. Delete the
   replaced check selectors, Python validators/wrappers and their tests.
   Move CI callers to the new checks in this PR. Remove `gate` rather than
   wrapping Cargo in the binary; update canonical evidence callers with the
   workflow owner. Remaining Python/TS security checks stay in CI explicitly.
3. **Hooks.** With confinement and workflow checkpoint APIs available, land
   retained hook handlers and recorded process tests; state owner lands
   registration edits in the same slice. Delete Python hooks/configurer and
   corresponding tests; remove discarded optional/post-merge registrations
   and caller docs. No reader of old transcript/state roots.
4. **Area bindings.** PR-sized owner slices connect dispatch/route,
   worktree/cleanup, Fabric/MCP, telemetry and workflow operations. Each
   slice lands the typed owner, CLI match arms and caller changes and deletes
   its Python/TS subject/test/helper inventory. No dummy success, unsupported
   command placeholders or compatibility dispatcher in a released build.
5. **Final CI/cut-over gate.** Once tracked Python/TS target sets are empty,
   delete their security/date/layering tooling, interpreter helpers and npm
   retry path; remove obsolete CI jobs and prove the Rust replacements on
   both OSes. The chairs perform the separately authorised installation only
   after all end-to-end gates pass. Every build PR reports `Net lines:` across
   Python, TS and Rust, names any growth and leaves main's Rust workspace green.
