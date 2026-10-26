# Command surface

## Purpose and ownership

`disk report` adds a read-only storage inventory using the existing globals and
result envelope. Its classes, JSON fields and measurement limits are specified
in [Disk report](disk.md).

People, skills, hooks and CI use one executable, `provenant`, to start work,
inspect durable results and apply policy checks. The `cli` crate parses
arguments, calls typed library operations and renders typed results; the binary
composes the library owners. Provider execution, custody, storage and Fabric
policy stay in their libraries. The [cross-cutting
decisions](README.md#cross-cutting-decisions) bind this spec, and it owns the
command contract every other area cites.

| Area | Owns | This spec owns |
| --- | --- | --- |
| [Run](run.md) | Admission, registry, attempts, status, retries and publication | The grammar of `dispatch`, `lanes`, `output`, `cancel`, `resume` and `reconcile` |
| [Routing](routing.md) | Catalogue, pools, quota and accounts | The grammar of `route`, `quota` and `accounts` |
| [Fabric](fabric.md) | Messaging, tasks, graphs, claims, landings, peer hosts and MCP | Only the mount point of `fabric` |
| [Workflow](workflow.md) | Review coverage, receipts, session checkpoints and writing rules | Only the mount point of `workflow` and the hook and check wiring |
| [Supervisor](supervisor.md) | Process custody, confinement and worktrees | The grammar of `worktree` |
| [State](state.md) | Roots, config files, install and release layout | The grammar of `root`, `config`, `install` and `uninstall` |
| [Telemetry](telemetry.md) | Reducer, report shape and trace export | The grammar of `telemetry` |

`provenant help` is the authoritative catalogue of what exists. This spec
states the rules the catalogue cannot, and the table below is checked against
it by the grammar tests.

## Boundaries

- Help, `--version` and `root` are cheap and read-only. They resolve no
  checkout, open no database, start no thread and probe no provider.
- No arguments prints usage. MCP is never selected by environment; it starts
  only through `fabric mcp`.
- Neither `fabric` nor `workflow` parses the other's output. Both edges call
  typed library operations. MCP tools belong to Fabric and the CLI adds none.
- The CLI keeps no durable command journal. Idempotence belongs to run's
  operation IDs.
- No library renders stderr or chooses an exit status.

## Contract

### Command tree

One spelling per name: no aliases, abbreviated long options or short flags
other than what clap generates. Every leaf takes `--help`, and `help [COMMAND...]`
shows the help of a command at any depth. `help --json` lists the whole surface;
`help COMMAND... --json` and `COMMAND... --help --json` list only that command
and its subcommands. Unknown, duplicate singleton or conflicting options fail.
Only options marked repeatable may repeat.

```text
provenant
  help [COMMAND...]
  root
  hosts [doctor [SSH_ALIAS]]
  config show | set | unset
  install | uninstall
  accounts list | register | set
  chair switch
  bench mine | verify | run
  route list | resolve | set | remove | reset | pacing
  dispatch
  lanes
  output ATTEMPT
  cancel TARGET
  resume ATTEMPT
  reconcile
  fabric mcp | identity | graph | mail | task | events | claim | landing
  telemetry stats | trace
  quota show | display | tick
  worktree create | list | remove
  pr watch NUMBER | merge NUMBER --head SHA
  disk report
  workflow review | receipt (show | save) | session (save | show | handoff)
  check config | install | harness | release | writing
  hook claude (pre-tool-use | pre-compact | compact | chair-prompt | chair-start)
  hook git pre-push
  internal supervise                  [hidden; launch handshake only]
```

Global options are accepted before or after the subcommand:

| Option | Meaning |
| --- | --- |
| `--help`, `--version` | Pure output, exit 0, except that `--help` on a hidden command (`internal` and its subcommands, `fabric peer`, `fabric peer-agent`) exits 2 as an unknown command. |
| `--json` | One versioned result, including errors. Refused before stdin for hooks and `fabric mcp`. Without it, `hosts`, `worktree`, `disk report`, `config show|set|unset`, `workflow` and the `fabric` leaves other than `mcp`, `peer` and `peer-agent` print terse text: one `key=value` line per record. `hosts doctor` and the remaining leaves print their own text. |
| `--state-dir ABS` | State root, overriding `PROVENANT_STATE_DIR`, then the default in [state](state.md). |
| `--project PATH` | The verified project to act on instead of the working directory. The process never changes directory. |

Environment inputs are `PROVENANT_STATE_DIR`, `PROVENANT_AGENT_LABEL` and
`PROVENANT_LANDING_TOKEN` for landing authentication. A development binary that
is not installed also needs an absolute `PROVENANT_ROOT` for its assets.
Provider and harness variables pass through as those tools define them.

Counts, durations and IDs are bounded or newtyped, and paths stay as raw bytes
until a record needs text.

### Waits

Every wait takes an explicit budget, spelt `--wait SECONDS`, from 1 to 14400
on the CLI and 1 to 55 over MCP, below the client's tool timeout. Omitting
`--wait` means no wait, and 0 is not a budget. There is no `--timeout` reader
flag. A wait ends successfully, exit 0, when its condition is met, including a
lane that needs input; the observed outcome is in `data`. Expiry exits 124 and
never cancels the underlying work. Cancelling a wait ends only that reader.

`pr watch NUMBER` is an exception: it delegates waiting to
`gh pr checks --watch`, with a 24-hour process limit and no `--wait` flag.
It reports pass or fail with the captured head and fails closed on incomplete
evidence. `pr merge NUMBER --head SHA` rechecks CI without waiting and guards
the merge with that full Git object ID. Both require at least one passing check,
with all others passing or skipped, and refuse a head change during verification.
See the [GitHub workflow](../runbooks/github-workflow.md#merge)
for the check and merge-author policy. Their JSON data contains `number`,
`head` and `result` (`pass` or `fail`); failures exit non-zero. A successful
merge also includes `merged: true` and reports `merged` in text output.

### Run verbs

Run owns the verbs at the top level because lane status is the busiest
surface. Fabric has no run verbs, and neither `fabric` nor `supervisor` adds
any.

`TARGET` is `RUN[/TASK[/ATTEMPT]]`. `ATTEMPT` is the full three-part form, and
commands that take it reject shorter targets. An ambiguous name fails with the
candidate IDs and never picks the newest.

Run scopes each `--operation-id` by caller and project and binds it to the
operation kind and a digest of the request. Repeating an ID with the same
request returns the recorded outcome or the in-progress receipt without
repeating effects. A different request is invalid, exit 2. This covers lost
replies and concurrent duplicates, including resume.

| Leaf | Grammar | Notes |
| --- | --- | --- |
| `dispatch` | One of `--prompt-file PATH`, `--prompt-stdin` or `--manifest PATH`; required `--operation-id ID`; route and access flags below; `--task-id`, `--delivery-id`, `--provider-timeout SECONDS`, `--concurrency N`, `--knob NAME=VALUE`, `--deliver-remote`, `--deliver-branch`, `--open-pr`, `--host`, `--branch`, `--base`, `--wait SECONDS` | Input is at most 1 MiB. A single prompt needs a route; a manifest conflicts with `--task-id`, `--delivery-id`, the route and access flags and `--provider-timeout`, which are per-task fields. A manifest holds 1 to 64 unique tasks as strict `{schema_version:1,tasks:[…]}` JSON; `--concurrency` is 1 to 8. Acceptance returns IDs, the exact route, state and artifact references after commit. |
| `lanes` | `[--id TARGET]… [--host HOST] [--all] [--state STATE]… [--since DURATION_OR_RFC3339] [--limit N] [--cursor TOKEN] [--wait SECONDS [--condition any\|all]]` | At most 100 IDs, limit 1 to 100 (default 50). `--all` conflicts with `--id`, `--cursor` conflicts with `--wait`, and `--condition` needs `--wait`. `--host` lists a configured peer's own lanes by label or host ID and conflicts with `--id`, `--wait` and `--cursor`. `--state` and `--since` filter the listing (since is a duration such as `24h` or an RFC3339 time, applied to queue time) and conflict with `--wait`. With no selector it lists the current agent's lanes in the project, excluding the caller and its ancestors. |
| `output` | `ATTEMPT [--part result\|prompt\|stderr\|events] [--offset N \| --tail N] [--max-bytes N]` or `ATTEMPT --trace` | Part defaults to result, offset to 0 and max bytes to 65536 (1 to 65536). `--tail N` reads the last N lines (1 to 500) and conflicts with `--offset`. The reply carries the next offset, EOF, encoding and completeness, so every unread byte stays reachable. Prompt reads need dispatch-equivalent project authority. `--trace` conflicts with the read options and prints the attempt's trace, then each parent's, one line per hop (a `trace` array in `--json`); the chain stops at a parent the local store lacks. Local runs only. |
| `cancel` | `TARGET --operation-id ID [--reason TEXT] [--wait SECONDS]` | Reason is at most 256 bytes. The reply states requested, already terminal or unobservable, with the custody observation. Acknowledgement is not process exit. There is no sibling or chair cancellation. |
| `resume` | `ATTEMPT --operation-id ID (--prompt-file PATH \| --prompt-stdin) [--wait SECONDS]` | Creates a new immutable attempt with the exact retained provider handle, route and access. It refuses `NONTERMINAL` while the target attempt has not finished, `UNSUPPORTED` when the adapter has no enabled resume recipe, the attempt has no recorded handle or a writer is active, and it refuses an attempt already continued, naming the newer one. A new route or a fresh turn is a new `dispatch`. |
| `reconcile` | `[--id RUN]…` | Asks run to recover queued and orphaned work idempotently for the selected runs, or the project's recoverable runs, and reports repaired, unchanged and refused items. At most 100 IDs. It takes no operation ID or wait. |

Route flags, shared by `dispatch`, `route resolve` and `bench`: one of
`--model ADAPTER/ID` or `--route NAME`, with optional `--effort`,
`--fallback ADAPTER/ID[,ADAPTER/ID]` (at most two, and not with a writer),
`--confidential` and `--allow-free`. Routing owns what they mean
([routing](routing.md)); no intent, reviewer, risk tier or
classifier selector exists.

`dispatch` alone takes `--account NAME`, pinning a registered account key or
label as a manifest task's `account` does. It conflicts with `--manifest`.

Access flags: `--mode read-only|worktree-write` (default read-only),
`--worktree PATH`, repeatable `--read-root PATH` and `--write-root PATH`,
`--network on|off` and repeatable `--capability postgres`. Network defaults to
on for writers and off for read-only lanes; off still permits the provider's
API endpoints. `postgres` adds localhost TCP and the Postgres socket without
opening outbound network. `--worktree`, or a peer writer's `--branch`, is
required with `worktree-write` and valid only with it. Roots must be existing
directories clear of the state root, at most 32 read roots, 32 write roots and
16 capabilities. Confinement refuses credential, broad and Git-internal roots
and rejects overlapping writer scopes before admission ([supervisor](supervisor.md)).
Nested dispatch and `--host` take the defaults and refuse overrides.

`--provider-timeout` is 1 to 14400 seconds, default 3600 for a read-only lane
and 10800 for a writer. `--host` places the dispatch on a configured peer
([fabric](fabric.md#peer-hosts)): a host that names this machine runs locally,
a peer writer takes `--branch` and `--base` instead of `--worktree`, and a
writer on any other host fails `UNKNOWN_HOST`. `--deliver-remote`,
`--deliver-branch` and `--open-pr` ask the host to fetch and push a writer's
final head and open a pull request after verifying it.

### Other leaves

| Leaf | Grammar | Notes |
| --- | --- | --- |
| `root` | no flags | Reports the asset root, state directory, selected project and installation identity. Human output is the newline-terminated asset path that scripts read. Read-only; an explicit `--project` verifies the checkout with the supervisor's bounded Git runner. |
| `hosts` | no flags | Lists the local host and configured peers with peer and adapter readiness observations. Read-only. |
| `hosts doctor` | `[SSH_ALIAS]` | Probes sign-in on one host, or every configured host, with no inference and no credential access. |
| `config show` | `[--task-class NAME]` | The resolved config with secrets redacted and each knob's source (default, profile or override). |
| `config set` | `NAME VALUE` | `NAME` is `profile` (balanced, economy or rigorous) or a registry knob. `null` clears a nullable knob. Validates against `config/harness-knobs.json` and rewrites `config.toml` atomically. |
| `config unset` | `NAME` | Removes a knob override. |
| `install` | `--source ABS --harness NAME… [--bin-dir ABS] [--hooks on\|off] [--adopt-existing] [--dry-run]` | Harnesses are claude, codex, cursor, agy and opencode. Omitting `--hooks` refreshes unchanged owned hooks and relinquishes edited entries; a first install leaves them disabled. Explicit `off` removes unchanged owned hooks. `--source` is a staged release. `--dry-run` writes nothing. See [state](state.md) for the layout and recovery. |
| `uninstall` | `--harness NAME…` | Removes only still-owned entries. |
| `accounts` | `list`; `register LABEL --adapter A --directory DIR [--plan] [--priority] [--billing-anchor]`; `set ACCOUNT [--display-name] [--hidden true\|false] [--order N] [--headline auto\|5h\|weekly\|default]` | Semantics in [routing](routing.md). `set` needs at least one option; an empty name, `--order 0` and `--headline default` clear a field. |
| `route` | `list [--adapter] [--pool]`; `resolve` with route flags; `set POOL MODEL [--weight 0..100] [--effort] [--efforts]`; `remove POOL MODEL`; `reset POOL`; `pacing PROVIDER pace\|spend\|max` | `list` and `resolve` are pure previews. The others edit pool overrides in `config.toml`. |
| `quota` | `show [--adapter]`; `display [--headline auto\|5h\|weekly] [--icon most_constrained\|ACCOUNT]`; `tick [--adapter] [--trigger timer\|panel\|manual\|wake\|power] [--power ac\|battery\|low]` | `show` never refreshes. `display` with no options changes nothing. `tick` defaults to manual and ac, and the quota menu bar app calls it. |
| `chair switch` | `[--to ACCOUNT] [--dry-run] [--session-id ID]` | Checkpoints and continues under an allowed registered Claude account. A real run needs the session ID. When the flag is absent, it reads `CLAUDE_CODE_SESSION_ID`, then `CLAUDE_SESSION_ID`. |
| `bench` | `mine [--limit]`; `verify TASK`; `run --route ROUTE[@effort] [--split search\|acceptance\|held-out] [--limit]` | `--route` is required, `--split` defaults to acceptance (held-out is never the default) and limits are 1 to 100. |
| `telemetry` | `stats [--usage] [--since] [--until] [--run-id] [--detail]`; `trace [--out FILE] [--since] [--run RUN]` | `trace --out` defaults to `provenant-trace.json` in the current directory. Semantics in [telemetry](telemetry.md). |
| `worktree` | `create BRANCH [--base REV]`; `list`; `remove NAME [--discard]`; `remove --merged --integration REF (--branch NAME… \| --since OID)` | Semantics in [supervisor](supervisor.md#worktrees). |
| `gc` | See below. | Collect lane caches; opt into worktree and output deletion. |
| `disk` | `report` | Read-only storage inventory; see [disk](disk.md) for measurement limits and JSON fields. |
| `fabric …` | Fabric's own clap subtree, mounted unchanged | Leaves and requests are in [fabric](fabric.md#cli-and-mcp-surfaces). The CLI supplies only the global options, envelope, exits and help entries. `fabric mcp [--client claude\|codex\|cursor\|agy\|opencode]` serves MCP on stdio. |
| `workflow …` | Workflow's own clap subtree, mounted unchanged | `review --request FILE\|-`, `receipt show\|save`, `session save\|show` and `session handoff`; see [workflow](workflow.md#cli-and-hooks). |
| `workflow session handoff` | `--to HOST --account LABEL [--note TEXT]… [--attach PATH]… [--prompt-file PATH] [--no-checkpoint] [--provider claude] [--model MODEL] [--wip \| --allow-dirty] [--dry-run] [--no-launch] [--session-id ID]` | Hands the chair or task session to a fresh Claude session on a peer host. When `--session-id` is absent, it reads `CLAUDE_CODE_SESSION_ID`, then `CLAUDE_SESSION_ID`. `--wip` conflicts with `--allow-dirty`, attachments total at most 512 KiB and `--provider` accepts only `claude`. The target host must enable `fabric.accept_handoff`. |
| `check` | `config`, `install [--source ABS]`, `harness`, `release [--base OID --head OID]` and `writing PATHS… [--rules general\|academic\|legal]` | Each takes `--limit N` (1 to 100, default 50) and `--cursor TOKEN`. |
| `internal supervise` | hidden | Launch handshake for run's scheduler and attempt guardian. Takes zero or five private descriptor numbers; `--json` refused. |

`gc [--dry-run | --apply] [--idle-days N] [--free-floor SIZE]` deletes orphaned
and idle lane caches. Merged worktrees and declared output are previewed;
`--apply` enables their deletion. `--dry-run` previews every class. The macOS
installer schedules GC every six hours with `--apply`, controlled by `[gc]`.

GC fetches the upstream default branch and removes clean registered merged
worktrees with known-terminal lanes, their local branches and own caches.
Tips equal to upstream or on its first-parent history are kept. Configured
regenerable directories are pruned only for known-terminal lanes, excluding
dirty trees, tracked and keep paths, symlinks and writes within 24 hours.

Cache collection removes owners that are gone and caches with no file written
for N days (1 to 3650, default 7). Idle removal takes the existing writer lock
without waiting and releases it after the tombstone rename. A held lock keeps
the cache as live; a missing or unsafe lock, a reservation, or unverifiable
registration keeps it as unobservable. An optional free-space floor (`20G`,
or plain GiB) removes further eligible caches oldest-first by newest recursive
write until the cache volume reaches the floor or candidates run out. The same
writer and reservation protections apply. Reports include kept and removed
paths and bytes freed; pressure removals use reason `free_floor`, with
`free_bytes_before` and `free_bytes_after` observations. `--dry-run` marks its
estimated free-space result with `free_bytes_estimated`.

`gc` JSON uses `removed[].reason` values `orphaned`, `idle`, `tombstone` and
`free_floor`, and `kept[].reason` values `foreign`, `live` and `unobservable`.
`kept` includes non-foreign entries whose writer state prevents removal.
Consumers must tolerate reason values they do not recognise.
`worktree_cleanup[]` contains its own `dry_run` flag, `worktrees` (the removal
result), `output[]` with `path`, `removed` and `reason`, and optional
`upstream_error`. In previews, output reason `would_remove` and `removed: false`
mark eligible directories. `alerts[]` records low-disk alerts or failures.
Configured `gc.alert_below` emits a Fabric note and gc log message once per
observed dip; previews leave the persistent latch unchanged.

`chair switch` quotes each handoff argument in single quotes, and writes each
apostrophe and backslash as a double-quoted character, which sh, bash, zsh and
fish all read literally. The pane command is a `/bin/sh -c` script, so the
login shell, which may be fish, parses only that one quoted argument. The hook
command in the settings JSON uses the same quoting.

`worktree remove` takes either a `NAME` or `--merged`, never both, and `--discard`
only with a `NAME`. `--merged` requires `--integration` (a branch or remote-tracking
ref such as `origin/main`) and exactly one of `--branch` (repeatable) or `--since`. It has no remote deletion and no force.

### Results, exits and size limits

Human results go to stdout. Failures print `provenant: CODE: message` on
stderr with a bounded, redacted cause and repair. With `--json` every command
emits one UTF-8 object and a line feed:

```json
{"schema_version":1,"command":"lanes","status":"ok","data":{},"error":null}
```

Area fields (changes, partial coverage, diagnostics, IDs, provider exits,
observed run outcomes) live inside `data`, never beside it. `error` is null or
`{code,message,cause,repair}` with a nullable cause and repair. Findings carry
rule, path and line and never the matched secret bytes. MCP tool results use
the same envelope inside their JSON-RPC framing.

| Status | Exit | Meaning |
| --- | ---: | --- |
| `ok` | 0 | An accepted operation or successful query, including pending state, partial coverage and a wait whose condition was met. A failed run seen by a query is data. |
| `failed`, `refused` | 1 | The operation failed, a check returned findings, or policy refused. |
| `invalid` | 2 | Bad input or flags, a missing or ambiguous target, or an unsupported record or config version. |
| `error` | 3 | Cannot proceed: setup, I/O, contention, required evidence unavailable or a result too large. |
| `timeout` | 124 | A reader's wait deadline passed; the run continues. |
| `cancelled` | 130 | The invocation was cancelled; the owner acknowledges before custody is released. |
| `unavailable` | 4 | Reserved. The renderer maps it, but no command emits it today. |

Hook and Git protocol exits and MCP's JSON-RPC errors follow their own
protocols, not this table. An MCP wait that reaches its deadline reports
`timeout` in the tool result and keeps the server running.

| Result | Bound and oversize action |
| --- | --- |
| Any ordinary result, human or JSON | At most 1 MiB including the envelope and line feed. The renderer buffers and size-checks before writing stdout. An oversize result fails `RESULT_TOO_LARGE`, exit 3, with a bounded pointer to a published file or a paged-read command. It is never truncated and a mutation is never rerun. |
| Lists and check findings | Leaves with `--cursor` page to fit the cap and report `next_cursor` with total and coverage. Findings keep exit 1 on every page. A single item that cannot fit fails as above. |
| Output reads | The requested byte or line window, with next offset and EOF. This is a slice the caller asked for, not clipping. |

Where a result has no existing artifact or read API, the CLI publishes a
redacted overflow result through the supervisor's safe file API before it emits
the error. A publication failure is exit 3, and a committed mutation stays
committed. A JSON write failure exits 3 and cannot promise a complete
envelope; SIGPIPE on a read-only listing succeeds. A parse failure uses the
envelope when an exact `--json` appears before `--`; only that flag is
pre-scanned. Non-UTF-8 paths use the records' tagged `utf8` or `native_bytes`
form, and human output escapes the bytes.

### Hooks

One static registry in `tools/front-door/crates/cli/src/hooks.rs` drives
dispatch, the recorded fixtures and the installer's hook list; config receives
its registrations from the composition root and never imports cli. Hook stdout
carries only the protocol response, and no hook launches a provider.

| Harness | Command | Event | Host timeout | Behaviour |
| --- | --- | --- | --- | --- |
| claude | `pre-tool-use` | `PreToolUse` on `Bash` | None | Guard: denies unsafe commands (below). |
| claude | `pre-compact` | `PreCompact` | 5 s | Advisory workflow checkpoint findings; always exit 0. |
| claude | `compact` | `SessionStart` on `compact` | 5 s | Emits the session's published checkpoint as context, at most 8 KiB; always exit 0. |
| claude | `chair-prompt` | `UserPromptSubmit` | 10 s | Chair prompt handler. |
| claude | `chair-start` | `SessionStart` | 5 s | Chair session-start handler. |
| git | `pre-push` | `pre-push` | n/a | Refuses or allows a push. |

The guard reads at most 1 MiB under a one-second deadline and accepts
additive fields. Oversize, malformed, unreadable or timed-out input, and any
policy failure, deny. A denial is one JSON line with
`hookSpecificOutput.permissionDecision="deny"` and a reason, exit 0; if that
cannot be written the guard writes a bounded reason to stderr and exits 2.
Allow emits nothing. A valid non-Bash event makes no decision. Transport errors
never echo the input, and no runtime stdin is persisted.

Its policy refuses pattern and group process control (a targeted
`kill [-SIGNAL] PID` with a literal PID above 1 is allowed) and points to
`provenant cancel TARGET`, then applies workflow's guard rules to the simple
commands the shell lexer reports: `stash`, `reset-hard`, `clean-force`,
`force-push` (`--force-with-lease` passes), `no-verify` (opt-in),
`primary-checkout` and `sleep-poll`. Config enables them through
`[domains.guard] deny`; an omitted id disables that rule, an unknown id is a
schema error and unreadable config keeps every rule on. Only Claude carries
these rules. Confinement and custody stay authoritative whether or not a hook
is installed.

`pre-push` takes the remote name and location as arguments and
`local-ref local-oid remote-ref remote-oid` records on stdin, with SHA-1,
SHA-256 and zero deletion IDs. It prints nothing to stdout; findings go to
stderr, exit 1 refuses and exit 0 allows. It refuses on any inspection, read
or size failure. It guards default-branch deletion, non-fast-forward updates,
pushes from linked worktrees to the default branch and conflict markers,
reading Git evidence with inherited redirects disabled and doing no fetch.

Installer leaves match hooks by exact event, matcher and command, keep array
positions, and never rewrite foreign siblings. Only Claude registers harness
hooks; a request for another harness's hooks refuses before any write.

### Checks

`check` subjects return records-owned findings (`rule`, `path`, `line`,
`message`) with a count and pagination. Any finding exits 1. Malformed input is
invalid, exit 2; unreadable required evidence is error, exit 3. A cursor binds
the input digest, rule version and selectors and rejects a changed input.

| Subject | Scope |
| --- | --- |
| `config` | Typed TOML and asset references. |
| `install` | Managed identities and links, without repair. `--source` is a staged release, as for `install`. |
| `harness` | `SKILL.md` frontmatter, OpenAI sidecars and local assets, skill-route fixtures, Markdown links and anchors, GitHub issue forms, and `verified-against: package@major.minor` pins against `package.json` and Cargo dependencies. Covers tracked project skills and installed skill roots. Project inputs are traversed deterministically and symlink escapes refuse. |
| `release` | Legal files, forbidden private paths, symlinks, 5 MiB blobs, secrets and home paths, over the head tree and the ancestor range, including added-then-deleted blobs and author, committer and message bytes. `--base` and `--head` come together or not at all. Trusted Git reads disable config and object redirects, replacements, fsmonitor, textconv, external diff and lazy fetch. A missing or unreadable object never yields a clean result. No all-ref scan, fetch or rewrite. |
| `writing` | Workflow's writing rules over 1 to 256 paths. `--rules` defaults to general. |

Heavy checks, shell syntax and language runners stay in CI.

## External contracts

- Exit codes and the envelope are consumed by skills, hooks, CI scripts and the
  quota menu bar app, which calls `quota tick` and `quota display`.
- The line-feed-terminated asset path from `root` is read by scripts.
- Claude's hook protocol: the `PreToolUse` deny shape, `SessionStart` context
  and the exit 2 blocking-error fallback.
- Git's `pre-push` argument and stdin protocol.

## Failure and recovery semantics

Exit codes are the table above. Hooks fail closed for the guard and `pre-push`,
and open with a stderr diagnostic for the advisory compact hooks. Output reads
are repeatable by offset. A mutation whose reply was lost is retried with the
same `--operation-id` and returns the recorded outcome. `worktree create`
refuses with `PrimaryMoved` if the primary checkout's `HEAD` changed during the
add, and on a failed add it removes only what it made and reports whether the
rollback was complete. Reapply `install` to recover a partial install.

## Deliberate cuts

| Cut | Reason |
| --- | --- |
| `fabric dispatch`, `fabric run`, named sessions, root `run`, `batch`, `lane`, `reap` | The run verbs above cover them; Fabric has no run verbs. |
| `utility`, classifier and output-digest commands and hooks | Not built; no command or routing hook calls a model for classification. |
| Profile mutation, quota history and credit commands, public capabilities plumbing | No consumer. `config set` and `route` cover policy edits. |
| `telemetry` ingest and report aliases, markers, ledger import | The reducer reads run records directly. |
| Composite `gate`, schema and Ajv commands, `lint`, `clean`, `prune-branches`, `--repo` | Checks are separate subjects; `reconcile` and guardian settlement clean scratch; `--project` selects the repo. |
| Remote branch deletion in `worktree remove` | Confined Git has no network or credentials. |
| `commit-msg` hook, Codex or OpenCode hook registrations | No recorded protocol. Those lanes rely on the sandbox and `pre-push`. |
| Python or argparse emulation, implicit probes on every invocation | One native parser; help stays pure. |

## Verification

- Grammar tests assert every leaf's flags, bounds and conflicts against the
  clap model and `help --json`, and that every wait accepts 1 and 14400.
- Render tests assert the exit for each status, the 1 MiB cap with
  `RESULT_TOO_LARGE`, and that oversize never truncates.
- Hook tests replay the recorded fixtures `pre-tool-use.json`,
  `pre-compact.json` and `compact.json`, including the exit 2 fallback.
- Check tests cover pagination cursors and exit 1 on findings.
- CI runs the full suite; locally run only the touched crate's tests.
