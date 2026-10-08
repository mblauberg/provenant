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

   On `workshop`, use `local_host: "workshop"` and a `laptop` peer. Without a
   file, the local name is `local`. Missing `peers` or an empty object means
   local-only operation. Project keys are primary checkout paths relative to
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

Each doctor row reports reachability, protocol version, revision, the
home-relative primary project path and whether it exists, configured adapter
executables, sign-in status, confinement and Fabric MCP registrations. Linked
worktrees use their primary checkout's path; peers require the same path
relative to their own home. Paths outside home or through escaping symlinks
are refused.

Sign-in states are `usable`, `unusable` or `unknown`. Bounded CLI status probes
never launch a model or return account identifiers. Claude keychain failures
become `unknown` or `unusable`. Adapters without a conclusive non-interactive
status probe report `unknown`. Confinement uses the existing read-only planning
and sandbox probes; the executing dispatch owner checks adapter sign-in,
effective read-only confinement and requested browser socket paths before
launch. Socket checks measure an estimated v2 attempt temporary path
in bytes against the platform limit; a long project path can exceed it.
Registration reports the existing install check's Fabric-entry presence,
independently of whether the adapter executable is on PATH.

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
`cancel`, `resume`, `handoff`, `output`, `operation`, `git-upload`, `git-result`
and `git-download`. Git verbs require an explicitly configured project and
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
placement and sessions qualified with a host never fall back. A bare named
session keeps its local owner. Resume, handoff and cancel follow the owning
host, including qualified task selectors.

Dispatch and control requests receive a bounded operation ID. Supply
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
are refused. Prompt files use the existing protected prompt reader and cross
the boundary as text; credential files, hard links and final symlinks are
refused. The peer request limit also bounds transmitted prompts.

`lanes`, `status`, `watch` and `lanes --wait` read each host's own attempt
records. Rows include `host` and qualified IDs. On failure, rows preserve
`last_known_state`, `last_known_status` and `age_seconds` while reporting
`unreachable` for transport failures or `read_error` with the typed owner
error when the host is reachable; neither can satisfy a terminal wait.
Independent peer reads run concurrently. Default reads cap rows per host
and then apply the combined 20-row cap, prioritizing unknown launches,
failed nonterminal lanes and one failure row per host before active work.
Other cached terminal rows retain their terminal ranking, newest first across
hosts. Explicit IDs are resolved
individually by their owners without transferring unrelated history. Partial
reads update only the observed rows and preserve other snapshot ages. Output
stays on its owner and returns at most 20,000 bytes
per call with `next_offset` for continuation. Each host keeps its own SQLite
state, operation journal and last successful observations. Remote dispatch
returns after the owner starts; use status or a lane wait to await completion.

Codex, agy and Claude seats use the same `fabric_dispatch`, `fabric_batch`,
`fabric_runs`, `fabric_status`, `fabric_cancel` and `fabric_output` tools.
Dispatch/batch inputs accept `host` and `operation_id`; dispatch also accepts
`resume` and `handoff`. Qualified IDs route read/control tools to their owner.

## Transport a writer

Configure the project key on the receiving host too, even when it has no
placement default: `"projects": {"Repos/project": {}}`. Git transfer refuses
unconfigured repositories. Both hosts need full, registered primary checkouts
at that home-relative path; shallow repositories and grafts are refused.

Create a chair worktree with the normal helper, using its branch-derived name,
then dispatch it:

```sh
provenant worktree create --new-branch feat/example
provenant fabric dispatch --host workshop --mode worktree_write \
  --worktree .worktrees/feat-example --prompt-file brief.md \
  --adapter codex --id example-task --operation-id example-writer
provenant fabric lanes --wait --all example-task@workshop
provenant fabric fetch example-writer@workshop
```

Fetch also accepts a qualified run or task ID; a batch requires one writer task at a time.
The MCP equivalents are `fabric_dispatch`/`fabric_batch` with `host`, `mode`,
`worktree` and `operation_id`, followed by `fabric_fetch` with `id`. These
operations use the same owner for Codex, agy and Claude chairs.

Only the chair worktree's committed HEAD and its Git history travel. The reply
states that uncommitted changes were not transported. Bundles travel in 32 KiB
chunks, with a 64 MiB bundle limit, through the restricted peer entrypoint;
GitHub is not involved. References live under `refs/provenant/hosts/`. The
peer creates the worktree with its own helper, and its dispatch owner holds the
host-local writer lease and checks sign-in, confinement and capability policy.
Writer operation IDs are limited to 120 characters. Replays preserve the frozen
base and destination; changing host or request is refused, including after a
successful launch. An interrupted upload remains `launch_unknown` and resumes
from the frozen input before dispatch reconciliation.
An unreachable implicit placement may fall back before send; the writer operation
is then durably bound locally and retries cannot launch it again on the peer.

Fetch requires a terminal writer. The executing host holds the writer lease,
checks registered worktree and branch identity, a new head descended from the
base, and cleanliness using the normal claim verifier. Its receipt binds project,
operation, base, head, host and worktree. Each operation and head has an immutable export. Earlier verified results stay
fetchable after handoff; an unverified result from a superseded writer is
refused. The
chair imports it into an owned ref and checks the advertised and fetched heads
against that receipt. Its existing branch and dirty files stay untouched. The
reply names the imported ref for review or cherry-pick.

Resume and handoff keep the transported worktree. A new writer worktree requires
a new dispatch; explicit writer overrides in handoff are refused. Remote writer
named sessions are refused; use qualified run IDs for continuations. Reusing a
worktree from an earlier dispatch requires Provenant's recorded context, the
same branch, an exact base match, cleanliness and an available writer lease.
An unrelated or changed peer worktree is refused rather than reset or removed.
Continuations require current ownership; a resumed attempt reserves it through
publication and completion. Read-only handoffs leave writer ownership intact,
and rejected preflights restore their prior reservation. Ownership and transport
records live in separate host-state directories outside the writer's Git directory.

## Verify slices 1–3

Run the focused checks from the product checkout, one test process at a time:

```sh
(cd runtime/fabric && npx vitest run tests/hosts.test.ts tests/lane-wait.test.ts tests/named-sessions.test.ts tests/execution-lifecycle.test.ts tests/run-reader-events.test.ts tests/lanes-cli.test.ts tests/surface.test.ts tests/lean-surface.test.ts --maxWorkers=2)
python3 -m unittest discover -s tests -p test_fabric_hosts.py -v
.venv/bin/python -m pytest tests/test_dispatch_run.py tests/test_provenant_cli.py -q
npm --prefix runtime/fabric run typecheck
```

MCP tests exercise diagnostics, lanes, status, bounded output, cancel and
placement rejection for Codex, agy and Claude seats. Loopback peers use
separate homes, instance roots and project repositories. They verify detached
launch, response loss, reconciliation, qualified selectors, fallback, stale
snapshots and bounded waits without an sshd or live model. Existing suites
whose scratch directories assume no enclosing checkout need a temporary
directory outside the checkout when the sandbox permits it.

Slice 2 covers criteria 2–5 and 7, the lane portion of 10, placement checks
in 14 and lane-tool parity in 17. Criteria 13 and 16 retain the slice 1
contracts. Real-host sleep/connection-loss acceptance remains for the chair;
loopback evidence cannot establish sleep behaviour. Slice 3 covers criterion 6 and writer dispatch/fetch parity in criterion 17
with loopback peers, including unpublished input, dirty chair files, batches,
continuations and interrupted transfers. Criteria 8–9, 11–12 and the remaining
record/tool portions of 10 and 17 belong to slices 4–5. Two-host writer and
forced-command acceptance remain for the owner’s spare Mac.

The real-sshd test is opt-in through `PROVENANT_SSHD_TESTS=1`. Run it outside a
sandbox that prevents sshd from starting, with `/usr/sbin/sshd` available, to
verify the documented `command=` restriction. A skipped test leaves that
acceptance criterion pending; the loopback shim does not prove it.
