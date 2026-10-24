# Command surface

Status: **accepted command-tree freeze**, 2026-10-24. Covers
`provenant-cli` and the `provenant` binary. Author route:
`codex/gpt-6.1-sol@high`.

## Purpose and ownership

People, skills, hooks and CI use one executable to start work, inspect durable
results and apply policy checks. `cli` parses arguments and renders typed
results; the binary composes library owners and selects the transport.
Provider execution, custody, storage and Fabric policy stay in their libraries.
[PORTING.md v7](../PORTING.md), the [rewrite plan](../README.md) and the
[Cross-area decisions](README.md#cross-area-decisions) bind this design.
[ADR 0028](../../../adr/0028-rust-port-dependencies.md) supplies the dependency
allowlist. This document owns the command contract every area cites.

| Keep | Drop / replacement |
| --- | --- |
| Cheap help, root and build identity | Implicit Git/probe/stale-checkout work on every invocation. |
| Top-level dispatch and lane controls | `fabric dispatch`, `fabric run`, named sessions, root `run`, `batch`, `lane` and `reap`; use the run verbs below. Fabric has no run verbs. |
| Route listing/resolution and current quota show/tick | Profile mutation, quota history/credit commands and public capabilities plumbing. |
| Fabric messaging, tasks, identity and peers | Root host/peer/lease synonyms; redundant CLI forwarding paths. |
| `telemetry stats` | Ingest/report aliases, retro/change markers, ledger and ledger import. |
| Worktree create/list and scoped removal/merged pruning | Implicit post-merge deletion; separate PR polling wrapper. |
| Checks and recorded hook protocols | Composite `gate`, public utility/schema/Ajv commands, output-digest and roster-advice hooks, npm retry and Python emulation/oracles. |

Routing removes `task_classifier._run_model()`, model/shadow chains and their
`utility("extract", …)` calls/config/tests. No classification selector or
replacement model call remains.
Execution-route deletes `utility.py`, `test_utility.py` and `utility-schema.cjs`
in step 3, which removes `task_classifier`'s utility calls, gated on their last
importer. CLI removes utility verbs and the output-digest hook at that gate.
No Python bridge or placeholder command keeps it alive.

## Command tree and conventions

One spelling per name; no aliases, abbreviated long options or short flag
synonyms, including `-h`. Every leaf has `--help`; `help [COMMAND…]` uses the
same Clap model. `help --json` returns the catalogue with options, defaults,
conflicts and bounds. Help does not resolve roots, open SQLite, start threads,
probe providers or invoke a command. No arguments means usage, never implicit
MCP selected by environment. Clap derive is already allowed; add no
completion, colour or shell-parser dependency.

```text
provenant
  help [COMMAND…]
  root
  config show
  install
  uninstall
  route list | resolve
  dispatch
  lanes [--wait SECONDS]
  output ATTEMPT
  cancel TARGET
  resume ATTEMPT
  reconcile
  fabric …                                [fabric-runtime clap subtree]
    mcp
  telemetry stats
  quota show | tick
  worktree create BRANCH [--base REV]
  worktree list
  worktree remove NAME | --merged --integration REF (--branch NAME… | --since OID)
  workflow …                              [workflow clap subtree]
  check config | install | harness | release
  check writing PATHS… [--rules general|academic|legal]
  hook claude pre-tool-use | pre-compact | compact
  hook git pre-push REMOTE LOCATION
  internal supervise                      [hidden; launch handshake only]
```

The [reconciled leaf grammar](#reconciled-leaf-grammar) freezes every leaf's
positionals, flags, bounds and owner request, including state's install
leaves. Owner designs use the same spellings; no reservation remains open, and
none authorises an old verb. `fabric …` and `workflow …` are the boundary only.
Neither `workflow hook` nor `workflow lint` exists. MCP tools belong to
`fabric-mcp`; CLI adds none. Both edges call typed library operations, never
parse each other's output. The binary calls `fabric-mcp` directly for
`fabric mcp`.

Global options are accepted before or after subcommands:

| Option | Meaning |
| --- | --- |
| `--help` | Pure help, exit 0. |
| `--version` | Package version and embedded workspace source digest, exit 0; no root lookup. |
| `--json` | One versioned result, including errors. Invalid for hook/MCP execution; reject before stdin. Pure help remains available. |
| `--state-dir ABS` | Fresh-state root, overriding `PROVENANT_STATE_DIR`, then `~/.local/share/provenant/v1`. Refuse a root inside any legacy root. |
| `--project PATH` | Select a canonical verified project instead of cwd; no process chdir or product-checkout selection. |

Installed product environment inputs are exactly `PROVENANT_STATE_DIR` and
`PROVENANT_AGENT_LABEL`. The latter separates inboxes for agents of one
provider, replacing `AGENT_FABRIC_LABEL`. Provider/harness variables pass
through as those tools define them; there are no other `PROVENANT_*` or
`AGENT_FABRIC_*` inputs. For an uninstalled development binary only, state's explicit `PROVENANT_ROOT`
selects asset fixtures; an installed binary ignores it.
Config is one user-owned `config.toml`, optionally
with `profile =`; no overlays, layering, mutation verbs or per-knob variables.
`config show` renders the resolved typed config with secrets redacted.

Unknown/duplicate singleton/conflicting options fail. Only explicitly marked
selectors repeat. Durations/counts are bounded integers; IDs are newtypes;
paths stay `OsString` until a record requires text. Every wait requires an
explicit budget: CLI 1–14400 seconds, MCP 1–55 seconds. Omitting a command's
wait option means no wait; zero is not a wait budget. Every wait is spelt
`--wait SECONDS`, which supplies its own budget; no `--timeout` reader flag.
Expiry never cancels underlying work. A wait condition is satisfied by terminal
or `InputRequired` state; that wait exits 0, with the observed outcome in data.

`root` reports product asset root, state directory, selected project and
installation version/digest in JSON. Human output retains the newline-terminated
asset path used by script callers (state slice 3); non-UTF-8 bytes are escaped.
It is read-only: no database, provider probes or old-root discovery. Default
root reporting starts no threads or processes. An explicit `--project` uses
the supervisor's bounded Git runner with isolated configuration to verify the
selected checkout. Config owns resolution and installation identity.

### Run grammar

Run owns admission, registry, attempts and status. Admission and operation
insertion are atomic in run's durable store. `fabric-runtime` supplies agent
identity, host-local project selection and federation routing; it owns no
second run registry. Each command uses the caller's `ExecutionContext`;
selector resolution never infers authority from a flag.
Run scopes operation IDs by caller/project and binds each to the operation
kind and canonical request digest. Reusing an ID with the same request returns
the recorded acceptance/outcome (or current in-progress receipt) without
repeating effects; a different request is invalid/2. This covers lost replies
and concurrent duplicate submissions, including resume.

`TARGET` is an exact `RUN[/TASK[/ATTEMPT]]`; `ATTEMPT` is the full
three-part form, and commands taking it reject shorter targets. IDs are opaque
typed values. Ambiguous names fail with candidate IDs; never pick the newest
match. For `lanes`, no selectors means the current agent's lanes in the
selected project, excluding caller and ancestors. `reconcile` has the project
scope below and requires matching caller authority; it does not inherit
lane-selection defaults. Flags and bounds are in the reconciled table below.

| Verb | Behaviour |
| --- | --- |
| `dispatch` | One prompt or a manifest of 1–64 independent typed tasks with unique IDs and one host. Acceptance returns IDs, exact route, state and artifact references after commit. |
| `lanes` | Without wait, list active plus five recent terminal lanes; `--all` includes retained terminal lanes. Wait snapshots selected active/queued lanes plus unseen terminal/attention revisions at entry; later launches belong to the next wait. Empty selection succeeds immediately. |
| `output ATTEMPT` | Return byte offset, next offset, EOF, encoding and completeness; every unread byte remains accessible. Prompt reads require dispatch-equivalent project authority. |
| `cancel TARGET` | Scoped request returns requested/already-terminal/unobservable and custody observation. Acknowledgement is not process exit. No sibling or chair cancellation. |
| `resume ATTEMPT` | Input-required or terminal continuation creates a new immutable attempt with prior-attempt reference and the exact retained provider handle, route and access. Missing continuation capability or a conflicting active writer refuses. A fresh turn or new route is a new `dispatch`. No automatic writer replay or mutable completed receipt. |
| `reconcile` | Ask run to recover queued/orphaned scheduler work idempotently for the selected runs, or the selected project's recoverable runs; report repaired/unchanged/refused items. No healthy-provider signal, inferred completion or released custody without owner evidence. |

Single-prompt dispatch names its route on the command line. Routing owns
eligibility; providers detect the account label and run records it.
Confidentiality is always on, without a flag. Workflow owns assurance policy;
no account pin, intent, reviewer, risk-tier, orchestrator-family, native-seat
or classification selector is exposed here. `route resolve` takes the same
route inputs. Tick remains because the live quota-menubar LaunchAgent calls it
(QuotaModel.swift:123). No fallback pool exists. The selected project supplies
cwd. No raw provider argv or permission bypass. Confinement proves authority
and rejects overlapping canonical writer scopes before admission. A manifest is
strict versioned JSON with typed route/access fields per task, inline prompt
or prompt-file reference and optional delivery ID; run/records own its schema.
Facts are `Serialize` only and built at edges.

Unreachable hosts remain unknown and cannot satisfy an all-lanes wait.
Cancelling a wait ends only that reader. Seen revisions advance only for rows
successfully delivered; pagination/oversize never acknowledges unseen rows.
Run's cancel, resume, read_output and export APIs take `&ExecutionContext`;
event reads use cursors. No separate public status/wait/retry/batch tree is
needed: lanes, output, resume and manifest dispatch cover those operations.

### Reconciled leaf grammar

This table is the step 1 parser freeze. Global options, `--help` and the
envelope apply to every row unless stated. `…` marks a repeatable flag or
positional; nothing else repeats. `SECONDS` waits are 1–14400 and
satisfied as the run grammar states. Two shorthands recur:

- `ROUTE` = `(--model ADAPTER/ID | --route NAME) [--effort VALUE]
  [--fallback ADAPTER/ID[,ADAPTER/ID]]`, mapping to routing's `RouteRequest`.
  Omitted fallback is `Fallback::None`; the list holds at most two distinct
  models and conflicts with a writer.
- `ACCESS` = `[--mode read-only|worktree-write] [--worktree PATH]
  [--read-root PATH…] [--network on|off] [--capability NAME…]`, mapping to
  the task's `Authority`. Mode defaults to read-only and network to off.
  `--network` governs the task's own network access; the selected provider
  profile always permits that provider's API endpoints.
  `--worktree` is required with, and only valid for, `worktree-write`. At most
  32 read roots and 16 capabilities.

| Leaf | Grammar | Owner request type | Bounds, conflicts and reason |
| --- | --- | --- | --- |
| `help` | `help [COMMAND…]` | records `HelpCatalogue` | Pure; no root, SQLite, threads or probes. |
| `root` | `root` | core `Roots` plus config installation identity | No flags. Reports asset root, state directory, selected project and installation together; state's `--kind` is dropped because `--json` already separates fields and global `--project` selects. |
| `config show` | `config show` | config `load` → `ResolvedConfig` | No flags; secrets redacted. |
| `install` | `install --source ABS --harness NAME… [--bin-dir ABS] [--hooks on\|off] [--adopt-existing] [--legacy-checkout ABS] [--dry-run]` | config `plan_install(Release, Targets, …)`, then `install(plan, …)` | At least one harness; hooks default off. `--source` is a staged release. Dry-run writes nothing. State's grammar stands. |
| `uninstall` | `uninstall --harness NAME…` | config `uninstall` | At least one harness; unchanged owned entries only. |
| `route list` | `route list [--adapter ADAPTER]` | routing `RouteListQuery { adapter }` | Unpaged bounded catalogue; no probe. |
| `route resolve` | `route resolve ROUTE` | routing `RouteRequest` with a read-only `RoutingContext` | Pure; no admission or state write. |
| `quota show` | `quota show [--adapter ADAPTER]` | quota `QuotaQuery { adapter }` | No refresh. `--adapter` replaces quota's `--provider`, matching `route list`: one adapter key, one spelling. |
| `quota tick` | `quota tick [--adapter ADAPTER] [--trigger timer\|panel\|manual\|wake\|power] [--power ac\|battery\|low]` | quota `QuotaTick { adapter, trigger, power }` | Defaults manual and ac; owner wall budget at most 55 s, no wait flag. The Swift app's `quota forget` and bare `quota` calls retire at cut-over: a tick drops the snapshot of any account that detection no longer reports, so no forget verb exists. |
| `dispatch` | `dispatch (--prompt-file PATH \| --prompt-stdin \| --manifest PATH) --operation-id ID [--task-id ID] [--delivery-id ID] [ROUTE] [ACCESS] [--provider-timeout SECONDS] [--concurrency N] [--host HOST] [--wait SECONDS]` | run `submit(Submission { tasks: [TaskRequest], concurrency }, OperationKey, …)`; `TaskRequest { id, prompt, route, access, timeout, delivery_id }`; `--host` is Fabric placement | Input at most 1 MiB. A single prompt requires `ROUTE`; manifest conflicts with `--task-id`, `--delivery-id`, `ROUTE`, `ACCESS` and `--provider-timeout`, which are per-task fields. `--concurrency` is manifest-only, 1–8, default 4. `--provider-timeout` is the provider runtime budget, 1–14400, default 3600 read-only and 10800 writer; run's `--timeout` is renamed so `timeout` never means a reader wait, and `--admission-timeout` is dropped because run's admission deadline is not caller-set. `--write-root` is dropped: confinement grants writes only to the verified worktree. Wait uses `All` over the receipt's attempts. |
| `lanes` | `lanes [--id TARGET…] [--all] [--limit N] [--cursor TOKEN] [--wait SECONDS [--condition any\|all]]` | run `query(RunQuery)` or `wait(RunQuery, WaitBudget, …)`; `RunQuery { ids, states, cursor, limit, condition }` | At most 100 IDs; `--all` conflicts with `--id`. Limit 1–100, default 50. `--cursor` conflicts with `--wait`; condition defaults to any and requires wait. `--wait SECONDS` replaces `--wait --timeout` so every wait has one spelling. `--summary` and lane tails are dropped: no request field; `output` reads bytes. |
| `output` | `output ATTEMPT [--part result\|prompt\|stderr\|events] [--offset N \| --tail N] [--max-bytes N]` | run `read_output(AttemptRef, ArtifactPart, ByteRange)`; `ByteRange { start: Offset(u64) \| TailLines(u16), max_bytes }` | Part defaults to result; offset defaults to 0; tail is 1–500 lines with no default, so a bare read starts at byte 0 rather than run's 40-line clip. Max bytes 1–65536, default 65536. |
| `cancel` | `cancel TARGET --operation-id ID [--reason TEXT] [--wait SECONDS]` | run `cancel(AttemptRef, OperationKey, CancelReason, …)` | Reason UTF-8, at most 256 bytes. Aggregate targets expand to exact attempts. |
| `resume` | `resume ATTEMPT --operation-id ID (--prompt-file PATH \| --prompt-stdin) [--wait SECONDS]` | run `resume(ResumeRequest { target, prompt, expected_generation: 0 }, OperationKey, …)` | Prompt required, at most 1 MiB, matching Fabric's non-optional `prompt`. No `ROUTE`/`ACCESS`: run's fresh mode is dropped for Fabric's fixed-route request. The edge sends generation 0, so an attempt continues once; resuming an already-continued attempt refuses and names the newer attempt to continue. |
| `reconcile` | `reconcile [--id RUN…] [--abandon-uncertain]` | run `recover(ProjectId, OperationId)`; with the flag, `reconcile(ProjectId, OperationId, ReconcileDecision::AbandonUncertain)` | At most 100 IDs; the edge maps each run to its operation. `--abandon-uncertain` requires at least one `--id`. No `--operation-id` or budget flag: the owner API takes neither, and recovery is idempotent. |
| `fabric …` | Fabric's own clap subtree, mounted unchanged | fabric-runtime and fabric-mcp requests | Fabric freezes its leaves. CLI supplies only globals, envelope, exits and help catalogue entries; the mount lands with Fabric's crate, never as a forwarding route. |
| `fabric mcp` | `fabric mcp [--client NAME]` | fabric-mcp transport | Explicit command-tree leaf frozen in step 1; no positionals; `--client` is the selected harness from state's MCP registry. `--json` refused before stdin. Until the owner binds, returns owner-unavailable; remaining Fabric leaves mount with their owner's subtree. |
| `telemetry stats` | `telemetry stats [--since DATE_OR_RFC3339] [--until DATE_OR_RFC3339] [--detail]` | telemetry `stats(facts, &PriceTable, StatsWindow { since, until, detail })` | Dates mean midnight UTC; timestamps need an offset; `since < until`. |
| `worktree create` | `worktree create BRANCH [--base REV]` | confinement `worktrees::create(WorktreeCreate { branch, base })` | Creates `.worktrees/<BRANCH with / as ->`. An existing branch with `--base` is invalid; a new branch's base defaults to the project's `HEAD`, pinned and returned. `--repo` is dropped for global `--project`. |
| `worktree list` | `worktree list` | confinement `worktrees::list(VerifiedRepo)` | Unpaged bounded list; no `--repo`. |
| `worktree remove` | `worktree remove NAME` \| `worktree remove --merged --integration REF (--branch NAME… \| --since OID)` | confinement `worktrees::remove(WorktreeRemove::Name \| Merged { integration, scope })` | NAME conflicts with `--merged`. Scope is required, as supervisor states, rather than cli's optional scope. At most 100 branches. REF is pinned once. No force. |
| `workflow …` | Workflow's own clap subtree, mounted unchanged | workflow requests | As for `fabric …`. |
| `check config` | `check config [--limit N] [--cursor TOKEN]` | config validation | All checks: limit 1–100, default 50. |
| `check install` | `check install [--source ABS] [--limit N] [--cursor TOKEN]` | config `check_install` | `--source` is a staged release, as for `install`, not a source checkout; one spelling, one meaning. |
| `check harness` | `check harness [--limit N] [--cursor TOKEN]` | cli `HarnessCheck` over a verified project | Tracked inputs only. |
| `check release` | `check release [--base OID --head OID] [--limit N] [--cursor TOKEN]` | cli `ReleaseCheck { range: Option<(GitOid, GitOid)> }` | Both range flags or neither. |
| `check writing` | `check writing PATHS… [--rules general\|academic\|legal] [--limit N] [--cursor TOKEN]` | workflow writing matcher | 1–256 paths; rules default general. Workflow's `--rules` replaces `--profile`, which collided with config's `profile`. |
| `hook claude …` | `hook claude pre-tool-use \| pre-compact \| compact` | `HookSpec` handler | No flags; `--json` refused before stdin. |
| `hook git pre-push` | `hook git pre-push REMOTE LOCATION` | `HookSpec` handler | No flags; `--json` refused. |
| `internal supervise` | `internal supervise [FD FD FD FD FD]` | supervisor launch handshake | Hidden from help; zero or exactly five private descriptor numbers, each 3–2147483647; `--json` refused. Run's scheduler and attempt guardian both enter here; the handshake names the role. |

S2 currently transfers the channel, two directory handles and two lease handles
as five private argv values. Step 1 preserves that existing Rust custody handoff;
none is public or opens arbitrary authority. A supervisor follow-up should move
that descriptor transfer into its handshake so this leaf can take no arguments.

No `clean` verb exists: guardian settlement and `reconcile` recovery delete
disposable scratch, and history stays retained. No utility, classifier or
routing-hook leaf exists, so step 1 does not wait on execution-route's step 3
deletion of their Python.

### Results, exits and size limits

Human results go to stdout; failures use `provenant: CODE: message` on stderr,
with bounded redacted cause/repair. Findings expose rule/path/line, never
matched secret bytes. Internal R8 diagnostics stay separate; logging failure
cannot change lifecycle state. JSON is UTF-8, one object plus LF:

```json
{"schema_version":1,"command":"lanes","status":"ok","data":{},"error":null}
```

Every area uses this envelope, including MCP tool result data inside its
external JSON-RPC framing. Area fields (`changes`, `partial`, `diagnostics`,
IDs, provider exits, observed run outcomes) belong inside `data`, never beside
it. `error` is null or `{code,message,cause,repair}` with nullable cause/repair.
The following mapping is exhaustive; `error` status cannot choose two exits.

| Status | Exit | Meaning / data |
| --- | ---: | --- |
| `ok` | 0 | Accepted operation or successful query, including running/pending state, partial coverage and a wait satisfied by terminal or `InputRequired` state. Underlying failed runs are query data. |
| `failed` | 1 | Requested operation failed or a check returned findings. |
| `refused` | 1 | Policy/authority refusal. |
| `invalid` | 2 | Invalid input/flags, missing or ambiguous target, unsupported record/config version. |
| `error` | 3 | Cannot proceed: setup, I/O, contention, unavailable required evidence or result too large. |
| `unavailable` | 4 | `OWNER_UNAVAILABLE`: the leaf parsed fully, but its typed library binding has not landed in this build. No forwarding or placeholder success. |
| `timeout` | 124 | Explicit reader wait deadline; run continues. |
| `cancelled` | 130 | Invocation cancelled; owner acknowledgement precedes custody release. |

Exit 4 is reserved for an unbuilt command owner; this keeps it distinct from
operational error/3 and the retired lease wrapper's held-by-another-holder/3.
Partial data states coverage and unavailable sources;
if required evidence prevents the operation, use `error`/3. Hook/Git protocol
exits below and MCP's JSON-RPC errors obey their external protocols, not this
process-exit table. An MCP wait ending on its deadline reports `timeout` in
the tool result without ending the server.

| Result shape | Bound and oversize action |
| --- | --- |
| Any ordinary result, human or JSON | At most 1 MiB of encoded result bytes, including envelope/LF for JSON. Buffer/size-check before stdout. Any result exceeding this fails with `error`/3 (`RESULT_TOO_LARGE`) and a bounded pointer to an owner-published file or a paged-read command. Never silently truncate or rerun a mutation. |
| Lists/check findings | Leaves with `--cursor` page to fit the same cap with `next_cursor` and total/coverage data; unpaged owner-bounded lists fail as above. Checks retain exit 1 if any finding exists, including on later pages. If even one item cannot fit, fail as above with a file/paged-read pointer. |
| Output/artifact reads | Explicit byte/line window above, with next offset and EOF; this is a requested slice, never silent clipping. A non-list result over 1 MiB still fails with 3. |

The library publishes artifacts before returning references. Where a result
has no existing artifact/read API, publish a redacted overflow result through
supervisor safe FS in the new state root before emitting the bounded error.
A publication failure is I/O/3, never success; a committed mutation remains
committed. CLI owns no durable command journal.

No library renders stderr or selects exits. Parse failures use the envelope
when an exact `--json` occurs before `--`; pre-scan only that transport bit,
then let Clap validate. JSON help/version data is catalogue/build identity.
A JSON write failure exits 3 and cannot promise a complete envelope. SIGPIPE
on a human read-only listing succeeds; failure writing a mutation result is
I/O/3 after recording its outcome, never a second execution. Non-UTF-8 paths
use records' tagged `utf8`/`native_bytes` representation; human output escapes
bytes. No Python repr/surrogateescape/argparse emulation.

## Hooks and checks

One static `HookSpec` registry in cli drives parser dispatch, fixtures and
state/config's installer list. Protocol event names are command names; workflow
registers handlers here rather than adding verbs. The timeout column is a
registration value, not a claim about an existing host default.

| Harness | Command | Protocol event | Matcher | Host timeout | Handler |
| --- | --- | --- | --- | --- | --- |
| claude | `pre-tool-use` | `PreToolUse` | `Bash` | Unset; no outer host deadline | CLI stdin/deny codec; confinement shell-command policy, 1 s local read/policy budget. |
| claude | `pre-compact` | `PreCompact` | None | 5 s | Workflow checkpoint validation; CLI codec, 1 s local budget. |
| claude | `compact` | `SessionStart` | `compact` | 5 s | Workflow new-root checkpoint context; CLI codec, 1 s local budget. |
| git | `pre-push` | `pre-push` | None | Not a Git protocol field | Supervisor/confinement trusted Git evidence and refusal policy. |

Hook stdout carries only the protocol response, no ordinary result/logging.
Claude input is at most 1 MiB, detected by a bounded read of cap+1 with a local
deadline; accept additive fields and validate every field used. Never persist
runtime stdin. For matched Bash `PreToolUse`, oversize, malformed/unparseable,
unreadable or timed-out input **denies**, as does policy failure. Emit one JSON
line with `hookSpecificOutput.hookEventName="PreToolUse"`,
`permissionDecision="deny"`, `permissionDecisionReason`, exit 0. If the denial
cannot be written, exit 2 with a bounded stderr block reason as Claude's
blocking-error fallback; recorded fixtures must verify both paths. Allow emits
nothing, exit 0. A validated non-Bash event makes no decision. Deny wording:
“Direct process control is refused. Use `provenant cancel TARGET` for owned
work.” Transport failures name the bounded-input error without echoing input.
Confinement/custody remain authoritative even when a hook is absent.

The same handler then applies the workflow guard rules (#1319) to the simple
commands the shell-command lexer reports, so there is no second parser. Rule
ids are compiled: `stash` (bare `git stash`, `stash pop`, `stash push` without
`-m`), `reset-hard`, `clean-force`, `force-push` (`--force`, `-f`, `+ref`;
`--force-with-lease` passes), `no-verify`, `primary-checkout` (`git switch`, and
`git checkout` of a branch or revision, when the directory is a repository
whose `.git` is a directory; `git checkout -- PATH` passes) and `sleep-poll`
(`sleep` of 5 s or more). Each denial carries a one-line remedy. Config is the
closed `[domains.guard]` section, `deny = [rule ids]`, all on by default;
omitting an id disables that rule and an unknown id is a schema error. Unreadable
config keeps every rule on. Input the lexer cannot parse is denied by the
process-control check above, not by these rules. Codex and OpenCode have no
recorded `PreToolUse` registration, so only Claude carries the rules; their
lanes rely on the sandbox and the pre-push hook.

`worktree create` records the primary checkout's `HEAD` before `git worktree
add` and refuses with `PrimaryMoved` (refused outcome) if it differs afterwards
(#1331), so a lane cannot silently move or detach the primary. Dispatch needs
no change; it only sees the refusal.

`PreCompact` reads validated cwd/session identity and emits advisory findings,
always 0. `SessionStart` reads that session's already-published checkpoint from
the new root and emits at most 8 KiB context, otherwise nothing, always 0.
Workflow owns checkpoint publication; neither scans transcripts. Advisory
oversize/bad input emits only bounded stderr diagnostics and no context.
No hook launches a provider.

Git pre-push takes remote name/location argv and stdin records
`local-ref local-oid remote-ref remote-oid`; accept native SHA-1/SHA-256 and
zero deletion IDs. No stdout; stderr findings and 1 refuse, 0 allows.
Inspection/read/size failure refuses. Preserve default-branch deletion,
non-fast-forward, linked-worktree push and conflict-marker guards. No
`commit-msg` registration. Git launchers are quoted absolute `exec` commands;
foreign hooks are preserved or reported as conflicts.

Only recorded Claude protocols register today. No invented Codex/other harness
hook equivalents. State owns config paths/editing, including
`CLAUDE_CONFIG_DIR/settings.json`. Cut-over's compiled exact-command/link-target
list under `--legacy-checkout ABS` includes:

- Git links to `scripts/hooks/pre-push` (replace) and
  `scripts/hooks/post-merge` (remove), resolved through the verified Git hooks
  path, with `readlink` only for legacy links.
- Provenant's `lane_safety_guard.py`, `utility_output_hook.py`,
  `routing_context_hook.py`, `skills/session/scripts/state_check.py --hook`
  and `skills/session/scripts/compact_reinject.py` registrations; replace kept
  handlers and remove retired ones using exact command matches.
- Legacy skill/workflow links, MCP entries and instruction links from state's
  installer inventory. Preserve Herdr entries in `~/.codex/hooks.json` and
  `~/.claude/settings.json`, every foreign entry and array order.

The installer prints every harness-config change before applying it;
`install --dry-run` prints the same plan without writes. No directory scan or
substring match establishes ownership. `check install` verifies managed state;
`uninstall` removes only still-owned values. Reapply `install` for recovery.
These are build/cut-over requirements; this design lane never installs.

All `check` subjects accept `--limit N` (1–100, default 50) and `--cursor TOKEN`.
Each page deterministically rechecks the inputs; the cursor binds their digest,
rule/bundle version and selectors and rejects changes, without a private CLI
snapshot store. Checks return ordered records-owned
`Finding { rule, path, line, message }` plus count and pagination; any findings
exit 1. `check config` validates typed TOML/asset references; `check install`
verifies managed identities/links without repair. Malformed check input is
invalid/2; defects in the inspected config/install are findings/1, while
unreadable required evidence is error/3.

`check harness` inspects a verified project: external `SKILL.md` frontmatter,
OpenAI sidecars/local assets, JSON skill-route fixtures, Markdown links/anchors
and GitHub issue forms. Records supplies R6's external YAML subset reader.
Traverse tracked inputs deterministically and refuse symlink escapes.
Shell/remaining JS/Python syntax checks and runners stay in CI.

`check release [--base OID --head OID]` scans tracked files/raw HEAD for legal
files, forbidden private paths, symlinks, 5 MiB blob bounds, secrets and home
paths. Both flags are required together; scan the nonempty ancestor range and
head tree, including added-then-deleted blobs and author/committer/message
bytes. No all-ref scan, fetch, publish or rewrite. Trusted Git reads disable
inherited config/object redirects, replacements/grafts, fsmonitor, textconv,
external diff and lazy fetch; support loose/packed objects, linked worktrees
and native alternates. Missing/truncated objects, unreadable/bounded-out files
or non-UTF-8 content never yield a clean result. One byte-rule registry;
prefilters must prove a superset. Retired skill-path prefixes and `--history`/annotated-tag
scans are not ported.

`check writing PATHS… [--rules general|academic|legal]` calls workflow's
data-driven writing rules beside each `SKILL.md`, reuses `Finding` and exits 1
on findings. There is no `provenant lint`.
Date lint is a workspace test over Rust `#[test]` sources (ADR 0028 decision 7),
with injected clocks or a local `timebomb: allow REASON` for deliberate dates;
no `check dates`, Python AST or JS matching.

### Hook implementation choices

CLI step 3 replaces the hook entry points with native transports and the static
`provenant_cli::hooks::HOOK_SPECS` catalogue. Config receives its registration
values from the composition root; it never imports cli. The registry supplies
Claude event names, matchers, host timeouts, handlers and recorded fixture names.

The guard reads cap+1 bytes under one monotonic one-second deadline. Input or
policy failure emits Claude deny JSON with exit 0; a bounded nonblocking stdout
write that fails uses stderr and exit 2. Transport errors never echo stdin.
Confinement inspects bounded shell words for direct process control, including
paths, wrappers and quoted command bodies. Dynamic expansions and ambiguous
shell syntax receive a refusal. This deliberately conservative advisory policy
does not interpret arbitrary scripts or aliases; OS confinement and owned
process handles remain authoritative.

Checkpoint callbacks validate the host cwd and session, resolve an existing
project binding without writes and delegate to workflow. They use a one-day
freshness window, a one-second local deadline and workflow's 8 KiB encoded
context limit. Missing bindings/state produce only a diagnostic. No transcript
or old-root lookup occurs.

Pre-push reads trusted Git evidence with inherited redirects disabled, refuses
incomplete history without trusting shallow boundaries, and checks default-branch deletion, linked
worktree pushes to the default branch, non-fast-forward updates and conflict
markers. A missing remote HEAD falls back to local main/master; an unknown
default branch refuses. New remote refs exclude commits already reachable from
known remote refs. No fetch or other remote operation occurs.

Installer hook leaves are matched by exact event, matcher and command. An
optional numeric legacy timeout is accepted. Replacements keep their array
positions; retired entries are removed without rewriting foreign siblings.
Only Claude has recorded harness registrations. Unsupported harness hooks
refuse before writes. The legacy pre-push link is replaced only in the verified
hooks directory inside canonical `--legacy-checkout`; external hooksPath values
refuse and foreign Git hooks conflict.

The retired configurer, utility output hook, routing context hook and their
standalone tests were already absent at this slice's base. This slice removes
the lane guard and old Git hooks, retaining only the mixed test file's reap
subjects until their run-owned caller retires. The compiled legacy manifest
retains names solely as exact installer removal/replacement evidence.

## State and crate seams

CLI has no private database, cache, run registry or approval store.
[Cross-area decisions](README.md#cross-area-decisions) binds root/layout;
the [state design](https://github.com/mblauberg/provenant/blob/e0a3cf839757df0afd7e2efe8f7a87aec7f76bb4/docs/specs/rust-port/design/state.md)
owns detailed paths, subject to that binding table: one host root, top-level
`runs/`, `fabric/`, `install/`, `releases/` and `config.toml`, never per-project
roots. The global state-dir override applies to CLI, MCP and hooks. Runtime
never reads/migrates/deletes legacy state or transcripts. The only one-shot
state import is Fabric's open/blocked tasks and unacknowledged requests from
the last 14 days, read-only at cut-over; cursors start after import. No ledger
import. Old telemetry history stays on disk and OTel retires at cut-over.

`CommandResult<T>`, `Finding` and `HelpCatalogue` are version-1 records-owned
transport types. Strict owned input rejects unknown fields unless its owner
specifies otherwise; additive output fields are allowed within v1. Fact types
implement `Serialize` only. Owners commit atomic/transactional outcomes before
success is printed; ENOSPC/diagnostic failure never requests a provider stop.

`cli::parse(IntoIterator<Item=OsString>) -> ParseOutcome` produces typed
`Invocation { globals, command }`, pure help/version or usage error.
`cli::render(&CommandResult<T>, mode, writer)` and
`cli::exit_status(&Outcome)` are binary-edge functions. No library imports cli.
The binary's exhaustive match wires requests to owners; hook codecs live here,
policy with confinement/workflow. Expose registry data to the composition root
for config's installer; config never imports cli.

| Needed API | Supplying owner |
| --- | --- |
| Roots, project selection, install identity/assets, managed registrations | core/records/config (state) |
| Safe bounded process/FS APIs, custody, cancellation, trusted Git/worktrees | supervisor/confinement; no cli `Command::new`, raw descriptors or unsafe |
| Atomic admission/registry, attempts, waits, continuation, output and evidence packet | run; `AttemptReader::attempt(AttemptId) -> AttemptFact` and `attempts(filter)` carry all terminal/route/reviewer/output facts workflow needs; the CLI composes `attempts()` with `stats(facts, &PriceTable, window)` and adds the active count from `snapshot` |
| Route listing/resolution and quota show/tick | routing/providers/quota; the execution-route design owns typed requests and results, and the reconciled table freezes their flags |
| Terminal-record reduction with bundled prices | telemetry; `telemetry stats`, no ledger |
| Identity, federation, coordination and MCP lifecycle | fabric-runtime/fabric-mcp |
| Writing/check/review policy and new-root session checkpoints | workflow |

Move cli's clean/worktree/Git/safety policy to those owners. Config owns install;
retire reap/PR polling, projection/retry-install/pending/oracle/Unicode/stale
scaffolding. Run owns git_evidence_packet via supervisor safe utility reads;
legacy run-directory/receipt/manifest bookkeeping is cut in favour of run
records. Binary forbids unsafe; supervisor alone owns syscall
wrappers. No async runtime or daemon.

## Fixtures and shared test harness

External contracts are provider wire forms, harness hooks/config, `SKILL.md`
YAML, MCP JSON-RPC 2.0 stdio and Git. Old owned output/layouts are not oracles.
Store sanitised recordings under
`tools/front-door/tests/fixtures/external/{providers,hooks,mcp,git}/`, with a
manifest containing tool/version, date, direction, redactions and byte digest.
CLI owns hook codecs and **records real Claude PreToolUse, PreCompact and
SessionStart input first in build step 3**: use a disposable project and
temporary `CLAUDE_CONFIG_DIR`, a tee hook delegating to the recorded handler,
and a version manifest. Trigger actual protocol events; a synthetic payload
is not a recording. Keep raw captures temporary, redact before committing and
remove raw captures after verification. Any provider activity needed for
capture requires separate live-run authority; no stable settings are edited.

Before enabling any hook/provider/MCP registration, its edge owner supplies
real recorded input/output/exit fixtures. CLI's hook fixtures cover guard
allow/deny, oversize, malformed, read deadline and blocked-write fallback,
plus advisory events/config acceptance. Provider fixtures retain argv
boundaries, allowed environment keys, stream bytes and terminal state, never
credential values. Git fixtures use real temporary repos, NUL-delimited
porcelain/object bytes and SHA-256 when supported. External YAML inputs use
`include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/…"))`.

Shared **test** support in the binary's `tests/support/` provides:

- `TestRig`: temporary HOME, XDG config/cache/state roots, disposable Git repo,
  fixture registrations and explicit `--state-dir`. Set `CLAUDE_CONFIG_DIR`,
  `CODEX_HOME` and `GIT_CONFIG_GLOBAL` to temporary config paths;
  `GIT_CONFIG_NOSYSTEM=1`, so system Git config is disabled. Clear ambient
  product/provider credentials/config redirects; allow only fixture values.
  Set `PROVENANT_STATE_DIR`/`PROVENANT_AGENT_LABEL` explicitly. Sentinel legacy
  roots remain byte-for-byte unchanged. Canonicalise macOS temp paths.
- `FakeProvider`: a `harness=false` integration test target. Without a scenario
  argument, its `main` runs the process-test suite; scenario mode emits fixture
  behaviour. The suite obtains `current_exe()` and passes that exact executable
  and scenario argument to its rig for re-execution. All fake-provider process
  scenarios live in this target, avoiding libtest argument handling, Cargo
  output parsing, guessed `CARGO_BIN_EXE_*` paths or extra product binaries.
  Scenarios cover malformed/truncated streams, output floods, input-required,
  silence, exits and descendants, recording only fixture argv/environment.
- `TestRig::command`: invokes `env!("CARGO_BIN_EXE_provenant")` through
  supervisor's safe runner, separately captures bounded stdout/stderr/status.
  All test child creation uses that API. `McpClient` drives initialise,
  initialised, list/call, correlation, cancellation and EOF; bounded draining
  threads keep floods from deadlocking and assert protocol-only stdout.
- FIFO/barrier handshakes establish spawn/ready/terminal/cleanup ordering, with
  watchdogs rather than sleeps. Explicit teardown cancels only owned processes,
  waits for terminal publication and reaps last. A parent rig retains custody
  when intentionally crashing a command child; abort cleanup never relies on
  Drop. Use the in-house snapshot helper, not insta.

Unit tests cover parse conflicts/bounds, all status/exit/oversize rows, partial
and InputRequired data, byte paths, scanners, YAML bounds and injected clocks.
Integration tests drive every accepted leaf/help entry, both wait maxima,
no-root help, cheap root, config/install/hook isolation and real protocol
fixtures. Linux/macOS process tests prove no healthy provider is killed by
silence/bookkeeping/ENOSPC, one writer, scoped cancellation, retained custody
on controller/MCP EOF, terminal publication despite delayed readers/controller
exit, and PID-reuse-safe cleanup. Re-express these before deleting lane
conformance; never compare with Python/TS.

CI owns full workspace/process tests and clean macOS release build without
Python/Node product dependencies. Locally run only fast targeted checks.
R7 compile/Clippy negative fixtures reject unsafe outside supervisor::sys and
direct process creation outside supervisor. Preserve workflow security lint,
exact-head checks and fail-closed `ci-status` with its updated job graph.

## Deletion accounting

Counts below are physical lines (including comments/blanks) at source baseline
`1b3d48e4c549811ce3636edc30b047f36fd24ada`, not promises to retain old test
assertions. The build deletes each
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
| `scripts/check_target.py` (baseline only; retired by state slice 3, with checkout selection in Rust) | 164 |
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
| state | `scripts/lib/front_door_build.py` (249); `scripts/install-provenant-command.py` (272); `scripts/lib/product_root_resolver.py` (107); `tests/test_front_door_build.py` (342); `tests/test_install_provenant_command.py` (272); `scripts/install-harness` (702); `tests/test_install_harness.py` (2,177; OTel tests retained separately) |

State retains `provider_registration_status` in `scripts/check-provenant-install.py` and its query tests while the live Fabric host importer remains. The CLI exit-3 checker stub is removed with the state cut-over.

| supervisor | `scripts/clean.py` (953); `scripts/worktree.py` (982); `scripts/reap.py` (258); `tests/test_clean.py` (1,526); `tests/test_worktree_policy.py` (1,018); `tests/test_run_artifact_cleanup.py` (199) |
| run (Git reads via supervisor) | `scripts/git_evidence.py` (341); `tests/test_git_evidence.py` (346) |
| workflow | `scripts/provenant_lint.py` (207); `tests/test_provenant_lint.py` (231); `scripts/provenant_pr.py` (223); `tests/test_provenant_pr.py` (186); `skills/session/scripts/state_check.py` (282); `skills/session/scripts/compact_reinject.py` (169); `tests/test_session_state_check.py` (509); `tests/test_ci_repository_assurance_policy.py` (772) |
| execution-route (step 3; last-importer gate above) | `scripts/utility.py` (241); `tests/test_utility.py` (154); JS helper `scripts/utility-schema.cjs` (16); `scripts/task_classifier.py` (190); `tests/test_task_classifier.py` (107) |
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

Step 1 also retires the remaining `tests/golden/` CLI judge with its router/help
subjects under R10. This step removes its CI invocation, golden variables
and unused Python-shard release build under the shared deletion rule. The
`PROVENANT_PORT_*` variables remain for live domain oracles; `ci-status`
continues to depend on every surviving job.

## Build order

The [reconciled leaf grammar](#reconciled-leaf-grammar) settles every owner
grammar, including execution-route's classifier removal and route inputs and
supervisor's worktree flags. Step 1 freezes the parser from that table.

1. **Parser and rig.** Land the frozen Clap model, exhaustive composition,
   renderer/exit/oversize contract and isolated shared rig. Delete the shell
   router, hand-written JSON/help, Python launcher and cli pending/oracle/
   projection scaffolding; no forwarding routes for unbuilt owners. Root/help
   tests replace delegation tests. Remove cli's last `// Rewrite pending`
   disallowed-process allow under R7 when cli is rewritten; require no such
   allow anywhere before the final gate. No stable install.
2. **Checks.** With records/config and safe Git/FS, land all check subjects and
   the Rust date workspace test. Delete replaced selectors/validators/wrappers
   and their tests, update CI/writing callers with workflow, remove `gate`.
   Keep remaining Python/TS checks only while their subjects exist.
3. **Hooks.** First record the three real Claude events with temporary config,
   tee hook and version manifest as specified above. With confinement and
   workflow checkpoint APIs, land codecs/handlers and recorded process tests;
   state uses the same HookSpec registry for installer edits. Delete old hooks,
   configurer/tests and retired registrations/docs; hand state the exact legacy
   git links/commands and Herdr preservation assertions. No old-root reader.
4. **Area bindings.** Connect run/route, worktree/cleanup, Fabric/MCP, telemetry
   and workflow in owner slices with caller updates and subject/test deletions.
   Execution-route's step 3 still gates utility deletion; its flags and the
   worktree flags are already frozen. No placeholder success or compatibility dispatcher ships.
5. **Final CI/cut-over gate.** Replace custody invariants before deleting
   `tests/golden/`, `tests/mcp_judge/`, `tests/lane_conformance/` and
   `tests/test_record_schemas.py` with their subjects. Remove their judge CI
   steps and their subject-specific variables in `.github/workflows/ci.yml`.
   Keep `PROVENANT_PORT_*` while quota, routing, workflow and fabric-runtime
   differential oracles still use them. Assert no `// Rewrite pending` allow remains.
   When Python/TS target sets empty, remove their security/date/layering tools,
   interpreter helpers, npm retry/jobs and obsolete setup/smokes. Rust negative
   lint fixtures, integration/MCP/install smokes and both-OS custody tests must
   pass in Actions; keep the aggregate gate intact. Chairs install only after
   the isolated end-to-end suite passes on both Macs. Every build PR reports
   Python/TS/Rust `Net lines:` and explains growth. Lanes do not merge or arm
   auto-merge.

## Lane D command bindings (2026-10-24)

`config show` now uses the landed config owner and complete domain registry.
It reports effective values and origins, applies secret redaction, creates no
state and refuses malformed configuration rather than returning defaults.

The cut-over request restores the narrow `hosts doctor [SSH_ALIAS]` spelling
for Claude sign-in diagnosis. This supersedes the earlier removal of that
spelling for this diagnostic only. Without an alias, inspect the default
Claude identity and each `.claude-<label>` directory; each CLI status probe gets
that exact `CLAUDE_CONFIG_DIR`. With an alias, use bounded, noninteractive SSH
to `~/.local/bin/provenant`, the default stable remote Rust front door.
A custom remote bin directory is not resolved by this diagnostic. Do not silently fall back to local
results when a remote host cannot answer.

Doctor runs `claude auth status --json`, never a prompt. It emits only identity
labels and typed states: `signed_in`, `signed_out`,
`credential_store_present`, `keychain_unavailable`, or `unknown`. A negative
login bit alone cannot establish sign-out: credential-file metadata or a
password-free keychain item-existence probe distinguishes a present identity
from an absent one. Store presence does not prove a token exists or can be
renewed. Keychain failure stays indeterminate. No token or raw
account/status output is persisted or printed. Every state carries
`token_validity = "unverified"`; local status cannot prove a token is accepted
by the provider or that quota and subscription information were refreshed.

## Final legacy cut (Lane F)

The cut-over chair authorises deletion of the entire Python orchestrator before
parallel run/dispatch replacement lands. Rust run/supervisor/confinement owns
custody and publication; no Python bridge or differential oracle survives.
The historical inventories above describe the source baseline, not files to
invoke. Current command grammar lives in `crates/cli/src`.

Decisions taken for the chair:

- Remove Node workspace bootstrap, package manifests/lock, test tooling and CI
  because no Node/TypeScript workspace remains. Remove the Python-only CI jobs,
  their shards, path filters and cached-tree markers. Every surviving Rust,
  harness asset and generic job runs on each tested tree; `ci-status` requires
  every one to succeed.
- Retire the old JSON record schemas and synthetic fixtures with their Python
  judge. Typed Rust records and owner boundary tests define current contracts.
- Delete both Git shell adapters. The merged CLI hook lane supplies runnable
  `provenant hook git pre-push` and Claude hook handlers with fixtures. Rust
  install owns registrations. Merged pruning uses explicit `provenant worktree
  remove --merged --integration OID --since OID`; the post-merge shell adapter
  and Python cleanup engine are retired.
- Delete `lease.py`: the run-directory driver lease is explicitly retired by
  execution-run. Rust custody is separate from `provenant fabric landing`.
  Delete `fabric_policy.py` with its Python differential test; the Rust
  fabric-runtime policy owner retains direct behaviour tests.
- Keep the three state/config Python helpers and the delivery/shared Python
  files assigned to lanes D/E. Their deletion is outside this lane's scope.
  [Remaining Python inventory](../remaining-python.md) lists every file and
  its owner. No TypeScript or JavaScript implementation remains.

Route attribution for this cut: `codex/gpt-6.1-sol@medium` via Fabric.
The author does not infer any other model identity. Chair review and green
exact-head CI remain merge gates; this lane does not merge.

Merge dependency: lane E must replace the delivery validator that imports
`scripts/git_evidence.py` and the deleted orchestrator helpers. These are
explicitly authorised parallel deletions, not an assertion that the retained
Python delivery implementation remains runnable. No shim is added.

The final main merge includes PR #1401's runnable Rust hooks and canonicalised
Git selection. Earlier review findings about unavailable hook handlers and
Homebrew symlink pinning are resolved by that owner, not waived by this cut.
