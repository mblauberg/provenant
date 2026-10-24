# Fabric hosts

Status: current
Applies to: `provenant hosts`, `provenant peer`, lane commands and Fabric MCP

Host diagnostics work over the user's OpenSSH configuration. Each host keeps
its own instance state and provider sign-ins. The [accepted
specification](../specs/fabric-hosts.md) and [ADR
0026](../adr/0026-federate-fabric-across-own-hosts-over-ssh.md) own the design.
Slices 1–3 provide configuration, diagnostics, remote lanes and writer code
transport. Messages, tasks and activity follow in slice 4; home-host landing
and work claims follow in slice 5.

## Configure each host

1. Install Provenant on both hosts using the [installation
   guide](../../README.md#quick-start). Verify the managed `~/.local/bin/provenant`
   shim works non-interactively. The host launcher uses the existing harness
   Python resolver; the product's Python environment must be installed.
2. Create `<instance-root>/.agent-fabric/hosts.json`. The instance root is
   `AGENT_FABRIC_INSTANCE_ROOT` when set, otherwise `~/.agents`. It must be an
   absolute path. Keep this machine-local file outside project content.

   ```json
   {
     "schema_version": 1,
     "local_host": "laptop",
     "peers": {
       "workshop": {
         "ssh_destination": "workshop",
         "peer_command": ".local/bin/provenant peer",
         "connect_timeout": 5,
         "response_deadline": 15
       }
     },
     "projects": {
       "Repos/provenant": { "default_host": "workshop" }
     }
   }
   ```

   On `workshop`, use `local_host: "workshop"` and a `laptop` peer. Host names
   are 1 to 63 lowercase letters, digits and hyphens. Without a file, or when
   `local_host` is invalid, the local name is the hostname as a slug: lowercased,
   cut at the first dot, other characters turned into `-`, so
   `Anes-MacBook-Pro.local` becomes `anes-macbook-pro`; an empty result is
   `local`. An invalid file is reported as `invalid_config`. Missing `peers` or
   an empty object means local-only operation. Project keys are primary checkout paths relative to
   home. An optional `modes` object overrides `default_host` for `read_only` or
   `worktree_write`. Defaults name
   the local host or a configured peer.
3. Configure the SSH aliases yourself. The destination may be an alias or
   `user@alias`; options, whitespace and shell metacharacters are refused.
   Host names use lowercase letters, digits and hyphens. Unknown JSON fields,
   duplicate keys, non-finite numbers (including exponent overflow) and invalid
   versions are refused. Both timeouts must be positive and at most 86,400
   seconds; connect timeout is an integer.
   `peer_command` and both timeouts are optional with the defaults above.
   A custom peer command must name one executable followed by `peer`, with
   no shell expansion or parent traversal. The default executable is relative
   to the remote user's home and needs no login shell.

## Restrict the SSH key

1. On each receiving host, add the sending host's public key to
   `~/.ssh/authorized_keys` yourself, with a fixed forced command and
   OpenSSH's `restrict` option. Substitute the receiving user's home-relative
   shim path and public key:

   ```text
   command=".local/bin/provenant peer",restrict ssh-ed25519 <public-key> user
   ```

   `restrict` disables forwarding, PTYs and user startup commands. The peer
   reads only its JSON request; it ignores `SSH_ORIGINAL_COMMAND` and never
   runs a command supplied in the request. Keep provider credentials on the
   host where that provider runs.
2. Verify the forced entrypoint with one closed JSON document:

   ```sh
   printf '%s\n' '{"protocol_version":1,"verb":"hello","params":{}}' |
     ssh -o BatchMode=yes -o ConnectTimeout=5 workshop .local/bin/provenant peer
   ```

   Expect one JSON response with `ok: true`, protocol version, host name and
   Provenant revision. If the peer's inherited PATH cannot resolve an adapter,
   doctor reports its executable as missing. Configure a fixed PATH in a
   user-owned peer launcher when needed; never rely on a login shell or
   interpolate the caller's command. Set `peer_command` to that launcher's
   home-relative path followed by `peer`, and restrict the key to it too.

## Diagnose

```sh
provenant hosts list
provenant hosts doctor
provenant hosts doctor --json workshop
```

Each doctor row reports reachability, protocol version, revision, Python version
and whether it meets the Python 3.11 minimum, the
home-relative primary project path and whether it exists, configured adapter
executables, sign-in status, confinement and Fabric MCP registrations. Linked
worktrees use their primary checkout's path; peers require the same path
relative to their own home. Case aliases that identify the same filesystem
directory are accepted. Paths outside home or through escaping symlinks are refused.

Sign-in states are `usable`, `unusable` or `unknown`. Bounded CLI status probes
never launch a model or return account identifiers. Claude keychain failures
become `unknown` or `unusable`. Adapters without a conclusive non-interactive
status probe report `unknown`. Confinement uses the existing read-only planning
and sandbox probes; the executing dispatch owner checks adapter sign-in,
effective read-only confinement and requested browser socket paths before
launch. Doctor's temporary-path `fits` estimate measures filesystem path and
component limits only. Browser lanes put Chromium's `SingletonSocket` under a
short private `/tmp/fb-<id>` directory (`MAC_CHROMIUM_TMPDIR`), so its path fits
the native Unix socket limit (104 bytes on macOS, 108 on Linux, including the
terminator) whatever the project path; remote placement still checks it.
Registration reports the existing install check's Fabric-entry presence,
independently of whether the adapter executable is on PATH.

Malformed `hosts.json` leaves `hosts list`, `hosts doctor` and `fabric lanes`
usable with local rows and a visible `invalid_config` note. Doctor exits nonzero
for invalid configuration or a failed Python minimum check. A peer unable to
start its interpreter reports that Python 3.11 or newer is required. Older
protocol-v1 peers without the runtime diagnostic require an upgrade to verify it.

Offline or timed-out peers get typed rows and leave the overall command
successful. A deadline marks only that host unreachable for that call.
Interrupt, termination and hangup signals cancel checks and reap their SSH
processes before returning.
`AGENT_FABRIC_SSH_PROGRAM` overrides the SSH executable for test shims.
Claude Code, Codex and agy MCP chairs call the same `fabric_hosts` tool with
`action: "list"` or `"doctor"` and optional `hosts: ["workshop"]`. Seat selection
does not change the host operation or require a Claude session. For example:

```json
{"action":"doctor","hosts":["workshop"]}
```

This invokes the same host client as `provenant hosts doctor --json workshop`.
The peer hello must identify the configured host, and a successful doctor
response must identify that host and the requested project path. Invalid
identity, version or diagnostic fields produce `bad_response`.

## Machine contracts

Protocol version 1 accepts one UTF-8 JSON document on stdin, at most 65,536
bytes, followed by EOF. The request has `protocol_version`, `verb` and optional
`params`. The fixed verbs are `hello`, `doctor`, `dispatch`, `lanes`, `status`,
`cancel`, `resume`, `handoff`, `output`, `operation`, `git-have`,
`git-upload`, `git-result` and `git-download`. Git verbs require an explicitly configured project and
accept the same `project_path`/`input` envelope; callers cannot choose a ref
namespace or shell command. Doctor accepts optional
`project_path`; lane verbs require it and accept an `input` object. There is
one newline-terminated JSON response on stdout:

```json
{"protocol_version":1,"ok":true,"result":{"protocol_version":1,"revision":null,"host":"workshop"}}
```

Errors have `ok: false` and `error: {"code": "…", "message": "…"}`, and the peer
exits nonzero. Stable request codes are `bad_json`, `request_too_large`,
`invalid_request`, `unknown_verb`, `protocol_mismatch`, `invalid_config`,
`invalid_project_path` and `doctor_unavailable`. The client adds `unreachable`,
`timeout` and `bad_response`. It bounds responses to 1 MiB and exchanges
`hello` on first contact. Different versions flag reads with
`reads_flagged: true` and `error.code: "protocol_mismatch"`, and set
`writes_allowed: false`. Dispatch and control callers use the shared
`protocol_decision` function and the client's `write=True` guard.

Lists use `fabric.hosts.v1`; doctor uses `fabric.hosts.doctor.v1`. The shared
identifier helpers parse and format `id@host`; bare record IDs mean local.
A selector matching records on multiple hosts returns `ambiguous_selector`.
Host command selectors are host names, optionally written `host@host`.

## Place and control a lane

```sh
provenant fabric dispatch --host workshop --prompt-file brief.md --adapter codex
provenant fabric lanes --json
provenant fabric status task-id@workshop
provenant fabric lanes --wait --all --timeout 120 task-id@workshop
provenant fabric cancel task-id@workshop --operation-id stop-review-1
provenant fabric output task-id@workshop --max-bytes 20000
provenant fabric dispatch --resume run-id@workshop --prompt-file follow-up.md
provenant fabric dispatch --handoff run-id@workshop --adapter claude --prompt-file handoff.md
```

`provenant dispatch` also accepts `--host` and `--operation-id` alongside its
existing owner arguments. Without `--host`, the per-project default applies.
An unreachable default host falls back locally only before a mutation is
sent; the attempt records `placement_fallback` and a warning. Explicit host
placement and sessions qualified with a host fail closed on
contact errors. Read-only placement whose committed-input preparation is
unavailable falls back locally before launch, including explicit placement;
the recorded reason identifies the transport failure. A bare named
session keeps its local owner. Resume, handoff and cancel follow the owning
host, including qualified task selectors.

Dispatch and control requests receive a bounded operation ID. Interrupting
CLI dispatch with Ctrl-C or SIGTERM stops the transfer child before it can
launch the peer lane and prints the operation ID. A lane whose launch was
already recorded keeps its normal detached lifetime. Supply
`--operation-id` to retry the same remote request; changing its content is
refused with `operation_conflict`. A lost response after send returns
`launch_unknown`. Subsequent lanes/status reads reconcile that ID against
the remote journal and owner record. Launch requests retry only when the peer
confirms the operation was never recorded. An unresolved cancel is checked
against its original attempts. Missing cancel, resume, handoff and named-session
operations are rejected instead of replayed against changed state. Prelaunch worker failures become
durable rejections. The peer journal stores request digests. The caller keeps
pending payloads until resolution. Both create the database with mode 0600.
Readers do not reconcile an operation while its original sender holds the
pending lock. Retrying a pending operation never falls back locally, even if
the default host is now unreachable or placement configuration has changed.

The remote peer starts the existing detached dispatch owner, then exits.
The lane survives the SSH connection. Paths in `cwd`, `worktree`, read roots
and additional directories are rewritten relative to home and resolved
against the receiving home. Paths outside home or escaping through symlinks
are refused. Prompt files may be in a scratch directory outside home: the chair
reads the explicitly supplied file with the protected prompt reader and sends
its content (up to 64 KiB), so no matching peer path is needed. They cross the
boundary as text; credential files, hard links and final symlinks are
refused. The peer request limit also bounds transmitted prompts.

`lanes`, `status`, `watch` and `lanes --wait` read each host's own attempt
records. Rows include `host` and qualified IDs. On failure, rows preserve
`last_known_state`, `last_known_status` and `age_seconds` while reporting
`unreachable` for transport failures or `read_error` with the typed owner
error when the host is reachable; neither can satisfy a terminal wait.
Independent peer reads run concurrently. Default reads cap rows per host
and then apply the combined 20-row cap, prioritising unknown launches,
failed nonterminal lanes and one failure row per host before active work.
Other cached terminal rows retain their terminal ranking, newest first across
hosts. Explicit IDs are resolved
individually by their owners without transferring unrelated history. Partial
reads update only the observed rows and preserve other snapshot ages. Output
stays on its owner and returns at most 20,000 bytes
per call with `next_offset` for continuation. Each host keeps its own SQLite
state, operation journal and last successful observations. Remote dispatch
returns after the owner starts; use status or a lane wait to await completion.

Codex, agy and Claude seats use the same `fabric_dispatch`, `fabric_runs`,
`fabric_status`, `fabric_cancel` and `fabric_output` tools. Dispatch accepts
`tasks[]`, `host` and `operation_id`, as well as `resume` and `handoff`.
Qualified IDs route read/control tools to their owner.

## Transport read-only input

Remote read-only dispatch transports the chair's committed HEAD using the same
bundle and chunk protocol as writers. The peer creates a detached worktree and
runs the lane there; uncommitted chair files do not travel. Both hosts need the
configured project key and a complete Git checkout. If preparation cannot
transport the input, dispatch runs locally with a visible `placement_fallback`
reason. The operation stays bound locally on retry.

The reader's prompt names its actual checkout, working directory and frozen
HEAD SHA before the original brief. Use those paths and HEAD when the brief
names paths or refs that exist only on the dispatching host. Targeted Rust tests
runs keep temporary files under the lane's writable `TMPDIR`; a Git discovery
boundary prevents scratch source fixtures from inheriting `.agent-run` ignore
rules and being classified as generated files.

The detached worktree is removed after the lane's owner settles; polling keeps
it while a fallback sequence is still alive and retries cleanup when needed.
Resume or inherited handoff recreates the
frozen detached input in a separate checkout per operation. Frozen reader input
metadata survives cleanup and lost launch replies. Writer worktrees remain
available for verification and fetch.

## Transport a writer

1. On both hosts, configure the project key, even when it has no placement
   default: `"projects": {"Repos/project": {}}`. Git transfer refuses
   unconfigured repositories. Both hosts need full, registered primary
   checkouts at that home-relative path; shallow repositories and grafts are
   refused.
2. Create a chair worktree with the normal helper, using its branch-derived
   name, and commit the base the writer should start from:

   ```sh
   provenant worktree create --new-branch feat/example
   ```

3. Dispatch the writer:

   ```sh
   provenant fabric dispatch --host workshop --mode worktree_write \
     --worktree .worktrees/feat-example --prompt-file brief.md \
     --adapter codex --id example-task
   ```

   The reply names the operation ID. Pass `--operation-id` only to retry the
   same request.
4. Wait for it to finish:

   ```sh
   provenant fabric lanes --wait --all example-task@workshop
   ```

5. Fetch the verified result. Fetch accepts the qualified operation, run or
   task ID; a batch requires one writer task at a time.

   ```sh
   provenant fabric fetch example-task@workshop
   ```

6. Land it. Fetch imports the head into
   `refs/provenant/hosts/<host>/<result>/result` and never moves a branch. Its
   reply carries `merge_command`, also printed in the digest, which
   fast-forwards the chair worktree:

   ```sh
   git -C ~/Repos/project/.worktrees/feat-example merge --ff-only \
     refs/provenant/hosts/workshop/<result>/result
   ```

The MCP equivalents are `fabric_dispatch` with `host`, `mode`, `worktree` and
`operation_id`, followed by `fabric_fetch` with `id`. For multiple tasks, pass
the per-task values in `tasks[]`. These operations use the same owner for
Codex, agy and Claude chairs.

### Errors

| Code | Fix |
| --- | --- |
| `project_not_configured` | Add the project key to the receiving host's `hosts.json`. |
| `worktree_required`, `worktree_invalid` (chair) | Pass a registered `.worktrees/<branch-derived-name>` linked worktree. |
| `worktree_invalid` (peer) | The named peer worktree is unrelated, dirty or off its transported base. Fetch or remove it on the peer, or use another branch. |
| `worktree_busy` | The named operation still owns the peer worktree. Wait for it, cancel it, or reconcile its operation ID with `lanes`. |
| `writer_context_superseded` | A later operation owns the worktree. Continue the newest run. |
| `code_transport_unsupported` | Unshallow the repository or remove its grafts. |
| `code_transport_too_large` | The bundle exceeds 64 MiB. Fetch the project's main branch on the peer first, so only newer history travels. |
| `code_transport_failed` | The message names the Git command, repository, exit status (and signal), any truncated output and the last error line. A chair input preparation failure before send, or a result fetch failure, can be repaired and retried with the same operation ID. A peer prelaunch rejection is journalled permanently: repair the peer, then dispatch with a new operation ID. |
| `timeout`, `unreachable` | A peer call or Git command did not finish. A chair input preparation failure before send releases the writer binding and frozen inputs: retry with the same ID, or dispatch locally. A peer prelaunch rejection requires repair and a new operation ID. After send with an unknown outcome, see `launch_unknown`. |
| `launch_unknown` | The request may have landed. Run `lanes`; it reconciles and resends frozen input only when the peer never recorded the operation. |
| `launch_not_started` | The peer stopped before launching. `lanes` or retrying the same operation settles it and retries owned worktree cleanup, including `locked (initializing)`. If `cleanup_pending` remains, inspect the preserved worktree and lock on the peer; avoid deleting unrelated or changed work. After settlement, dispatch with a new operation ID. |
| `code_input_incomplete`, `code_input_missing` | The committed input did not arrive whole. Dispatch again with a new operation ID. |
| `operation_conflict` | The operation ID already binds another request, host or input. Use a new ID. |
| `writer_not_terminal` | Wait for the writer to finish. |
| `writer_verification_failed` | Commit beyond the base, leave the peer worktree clean on its branch, and let the lane finish. |
| `writer_binding_missing`, `code_result_ambiguous` | Select one writer by its qualified operation, run or task ID. |
| `writer_head_mismatch`, `bad_response` | The peer's result contradicts its verification. Nothing was imported; investigate the peer. |

### What travels

Only the chair worktree's committed HEAD travels. The reply states that
uncommitted changes were not transported. After contact, the chair offers the
peer up to 64 first-parent ancestors of that base (`git-have`); the peer names
the nearest one a local ref already reaches, and the bundle carries only the
history after it. A peer with no shared history receives the full history.
Verified results return as base-to-head bundles. Uploads travel in 32 KiB
chunks and downloads in 512 KiB chunks, with a 64 MiB bundle limit, through the
restricted peer entrypoint; GitHub is not involved, and neither host needs
GitHub credentials. A peer that already fetched the project's main branch
therefore receives a few commits even when the full history exceeds the limit.
The peer publishes each chunk whole, so an interrupted upload resumes cleanly.
Each host deletes its copy of the input bundle once the peer has consumed it.
References live under `refs/provenant/hosts/`.

The peer creates the worktree with its own helper, without dependency
provisioning, and its dispatch owner holds the host-local writer lease and
checks sign-in, confinement and capability policy. Transport Git runs ignore
repository hooks, `core.fsmonitor`, filter drivers, replace refs, lazy fetches,
submodule recursion and automatic maintenance; the writer lane itself uses the
repository normally. Writer operation IDs are limited to 120 characters.
Replays preserve the frozen base and destination; changing host or request is
refused, including after a successful launch. An interrupted upload remains
`launch_unknown` and resumes from the frozen input before dispatch
reconciliation. A peer that stops before launching settles as
`launch_not_started` and releases its worktree claim. A failed prepare or an
unlaunched operation also removes any worktree and branch it created, provided
they remain clean at the transported base and no later writer owns them.
Settlement also repairs a half-created worktree marked `locked (initializing)`,
including one whose `.git` backlink was never published, when the operation
creation record, Git registration, branch and unchanged base prove ownership.
An unrelated lock, changed branch or live writer lease remains `cleanup_pending`.
An unreachable implicit placement may fall back only at initial placement; the
writer operation is then durably bound locally. An existing binding keeps its
host on every retry, including when the first explicit contact failed before send.
A failure while preparing the chair input, before anything is sent, retires its
frozen input records and owned refs before releasing the binding. Repair and
retry with the same ID, including on another peer. Once the peer records a
prelaunch rejection, that ID always returns the recorded rejection; repair the
cause and dispatch with a new operation ID. Result fetch retries use the same
writer selector and its frozen verification.

Each transfer is a sequence of peer calls bounded by `response_deadline`. For
large bundles, let SSH reuse one connection in the chair's `~/.ssh/config`:

```text
Host workshop
  ControlMaster auto
  ControlPath ~/.ssh/control-%C
  ControlPersist 60
```

### Verification and fetch

Fetch requires a terminal writer. The executing host holds the writer lease,
checks registered worktree and branch identity, a new head descended from the
base, and cleanliness using the normal claim verifier. Its receipt binds project,
operation, base, head, host and worktree. Each operation and head has an
immutable export. Earlier verified results stay fetchable after handoff; an
unverified result from a superseded writer is refused. The chair selects its
recorded input and continuation lineage before checking the receipt. It
verifies the downloaded pack and ancestry in a temporary object repository,
then imports only history reachable from the approved head into an owned ref.
Hidden packed objects and rejected heads stay outside the chair's object
store. Its existing branch and dirty files stay untouched until the operator
runs the fast-forward.

### Continuations and ownership

Resume and handoff keep the transported worktree. A new writer worktree requires
a new dispatch; explicit writer overrides in handoff are refused. Remote writer
named sessions are refused; use qualified run IDs for continuations. Reusing a
worktree from an earlier dispatch requires Provenant's recorded context, the
same branch, an exact base match, cleanliness and an available writer lease.
An unrelated or changed peer worktree is refused rather than reset or removed.
Continuations require current ownership; the shared launch owner checks that
ownership under the writer lease for any run a transported writer launched,
including local resumes on the executing host. An ordinary local writer that
finds a stale transported claim clears it. A resumed attempt reserves ownership
through publication and completion. Read-only handoffs leave writer ownership
intact, and rejected preflights restore their prior reservation. Ownership and
transport records live in separate host-state directories outside the writer's
Git directory.

### Clean up

Writer transport keeps its refs, worktrees and records until the operator removes
them. After landing a result:

```sh
git -C ~/Repos/project for-each-ref --format='%(refname)' refs/provenant/hosts/
git -C ~/Repos/project update-ref -d refs/provenant/hosts/<host>/<id>/<base|result>
```

Run both on each host. On the peer, remove the writer worktree with
`provenant worktree remove feat-example` and its branch with
`git branch -D feat/example` once nothing will resume it. Frozen inputs and
result bundles live under `<instance-root>/.agent-fabric/code/`; delete an
operation's directory there once it is fetched and no retry is pending.

## Verify targeted changes

Run focused Rust checks serially from `tools/front-door`:

```sh
cargo test -p provenant-fabric-runtime
cargo test -p provenant-fabric-mcp
```

Run only the crates touched by the change. Full workspace suites run in
GitHub Actions. The Python host owner, its tests and TS loopback tests are
retired; Rust boundary tests own current federation and MCP behaviour.
