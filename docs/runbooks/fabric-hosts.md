# Fabric hosts
Owner: provenant-fabric-runtime
Anchor: tools/front-door/crates/fabric-runtime/src

Fabric uses bounded SSH calls and host-owned state. Each host keeps its own
provider sign-ins. The [Fabric design](../specs/fabric.md#peer-hosts) owns the
peer model and boundaries.

## Commands

```sh
provenant help
provenant hosts
provenant hosts doctor
provenant fabric peer
provenant fabric peer-agent
provenant help run
provenant lanes --host workshop
provenant workflow session handoff --help
```

`hosts` reads local and configured peer health. `hosts doctor`
probes only Claude identities under `~/.claude` and `~/.claude-*`; it is not a
general provider sign-in check. Remote doctor uses
`~/.local/bin/provenant` on the peer. Peer calls run `provenant fabric peer`
through the peer's non-interactive `PATH`. The hidden `fabric peer` command is
the fixed SSH protocol entrypoint; the hidden `fabric peer-agent` command is the
macOS LaunchAgent that runs it in the login session (see below). Remote
dispatch uses top-level `dispatch --host`. The Rust CLI grammar lives in
`tools/front-door/crates/cli/src/grammar.rs` and
`tools/front-door/crates/cli/src/fabric.rs`.

## Local graphs

Use a graph for a writer followed by a review. Create and register the writer's
worktree first with `provenant worktree create`, then save the following
as a graph file in the project:

```json
{
  "schema_version": 1,
  "graph_id": "write-review",
  "nodes": [
    {
      "id": "write",
      "title": "Implement change",
      "prompt": "Implement the approved change and commit it.",
      "access": "worktree_write",
      "worktree": ".worktrees/feature",
      "route": {"alias": "senior"}
    },
    {
      "id": "review",
      "title": "Review change",
      "prompt": "Review the change in .worktrees/feature.",
      "access": "read_only",
      "depends_on": ["write"],
      "route": {
        "adapter": "claude",
        "model": "claude/claude-sonnet-5-5",
        "effort": "medium",
        "account": "pro"
      }
    }
  ]
}
```

Configure the `senior` routing pool and register `pro` with `provenant accounts
register` before running the graph. An alias-only route resolves its adapter
through the same routing configuration as `dispatch --route`. Explicit
adapter and model routes also work. A route must include an adapter or alias;
`route` selects one route, while `routes` provides an ordered fallback list
that can mix both forms. Both forms accept `account`. A node's optional `title`
appears in `lanes` and graph status; it defaults to the node ID and must pass
the task title validation, including the prohibition on control characters.

```sh
provenant fabric graph run --document graph.json
provenant fabric graph status write-review
```

The run command returns after starting ready nodes. Settlement automatically
starts dependants and updates the checkpoint to complete. `status` only reads
the checkpoint. Cancellation fences new starts; it remains in effect until
an explicit resume:

```sh
provenant fabric graph cancel write-review
provenant fabric graph resume --document graph.json
```

After a supervisor crash, the next settlement in the project advances pending
graphs. Use `resume` when nothing else will settle. Keep the original document
for that recovery command; changing its plan produces a conflict. If a graph
stays pending, `graph status` shows `advance_error` when an automatic advance
failed. Failed writers retain their checkout for inspection. See the
[graph spec](../specs/fabric.md#bounded-graphs-and-route-fallback) for fallback
rules and limits.

## Configure hosts

Install the Rust `provenant` binary on each host and make it available to
non-interactive SSH. Run `provenant fabric identity` on each host to inspect
its identity. Keep credentials on the host that runs the provider.

The selected state directory holds `config.toml`. Select it with
`--state-dir /absolute/path` or `PROVENANT_STATE_DIR`; the default is
`~/.local/share/provenant/v1`. Run `provenant config show` to inspect the
resolved configuration before changing it.

Peer configuration is under `domains.fabric`, not a separate `hosts.json`:

```toml
schema_version = 1
profile = "balanced"

[domains.fabric]
ssh_executable = "/opt/homebrew/bin/ssh" # optional; empty selects system discovery

[domains.fabric.peers.workshop]
host = "<peer host ID from fabric identity>"
destination = "workshop"
remote_executable = "/Users/user/.cargo/bin/provenant" # optional
connect_timeout_seconds = 10
```

Replace the placeholder with the peer's actual host ID. Configure the reverse
peer on the other host. `home_hosts` maps cross-host project keys to home host
IDs. Peer destinations are bounded SSH aliases or `user@alias`, with no shell
options. `ssh_executable` overrides system SSH discovery. A peer's
`remote_executable` selects the installed `provenant` binary; the transport
always appends the fixed `fabric peer` entrypoint. `connect_timeout_seconds`
must be between 1 and 30; `limits.peer_seconds` separately bounds the full peer
call. Executable overrides must be absolute, executable paths
without whitespace or shell metacharacters. `remote_executable` must exist
and be executable on the peer. The complete field validation and compiled
defaults live in
`tools/front-door/crates/fabric-runtime/src/config.rs` and its `assets/`.

Graph policy is configured separately from peer transport limits. For example:

```toml
[domains.fabric.graph.limits]
nodes = 512
edges = 4096

[domains.fabric.graph.limit_ceiling]
nodes = 1024

[domains.fabric.graph.budget.per_node]
tokens = 100000
cost_usd = 5.0
wall_time_seconds = 1800

[domains.fabric.graph.budget.per_graph]
tokens = 1000000
cost_usd = 50.0
wall_time_seconds = 7200
```

Limits also include `routes_per_node`, `node_prompt_bytes`,
`graph_prompt_bytes`, `expansion_depth`, `fan_out_width`, `loop_rounds`,
`continuations` and `concurrency`. The defaults are 16 routes, 256 KiB per
node prompt, 4 MiB per graph, expansion depth 4, fan-out 32, five loop rounds
and three continuations. When concurrency is unset, host lane caps apply.
The ceiling defaults to the same values as the limits and can be raised to
allow a graph document to request a higher value. Budget fields are optional;
unset fields have no budget. Costs are in US dollars.

A graph document can set a `limits` object with any of these fields. Values
must be positive and no higher than the corresponding configured ceiling.
When a structural limit or budget is reached, graph status reports `paused` and
the checkpoint records the reason. Running attempts can finish. Resume with,
for example, `provenant fabric graph resume graph.json --extend limits.nodes=600`
or `--extend per_graph.tokens=2000000`. Repeat `--extend` for multiple values.
Extensions set new absolute values, must increase the existing value, and are
recorded in the checkpoint. Limit extensions must also fit the configured
ceiling. Token counts and estimated cost use available attempt telemetry;
missing usage or prices cannot contribute to a budget. Wall time is checked
on graph advancement and attempt settlement.

## SSH and peer protocol

Configure SSH aliases and keys in the user's OpenSSH configuration. The Rust
transport uses `domains.fabric.ssh_executable`, which defaults to
system discovery and is resolved through the shared runtime helper. It sends the
fixed remote command `provenant fabric peer`, using the peer's
`remote_executable` when configured. Set an override when SSH is installed
elsewhere. A restricted key can use:

```text
command="/absolute/path/to/provenant fabric peer",restrict ssh-ed25519 <public-key> user
```

Verify that the receiving host can resolve the binary in this non-interactive
context.

### macOS peers and the login keychain

A process started from an SSH session on macOS cannot read the login keychain
(`security show-keychain-info` reports "User interaction is not allowed"), so
`claude auth status` reports no login for every Claude identity and Claude
routes fail with a login-required refusal. Codex is unaffected because it keeps
its credential in a file.

`provenant install` on a Mac, run for the account's own home and the default
state root, writes `~/Library/LaunchAgents/dev.provenant.peer.plist` and
(re)starts it in the GUI domain with `launchctl bootstrap gui/$(id -u)`. The
agent runs `provenant fabric peer-agent`, which listens on
`~/.local/share/provenant/v1/peer-agent/peer.sock` (directory `0700`, socket
`0600`). Each `fabric peer` call over SSH forwards its request to that socket,
and the agent runs it in a fresh `fabric peer --in-process` child inside the
login session, where the keychain is readable. When the agent is not
listening, `fabric peer` handles the request itself as before. A request that
reached the agent is never retried in process; a lost reply surfaces as an
error and, for a mutation, as `launch_unknown`.

Re-running install is idempotent: it rewrites the plist only when it changed
and restarts the agent so it serves the new binary. The plist's PATH starts
with fixed system directories, then the install bin directory and existing
absolute directories holding resolved Claude, Codex and Git executables.
Run install from a shell whose PATH finds those executables; other PATH
entries are dropped. Installs with another home or `--state-dir` skip
the agent. The agent logs failed requests to
`~/.local/share/provenant/v1/peer-agent/agent.log`.

Check it with `provenant hosts doctor` locally or `provenant hosts doctor
workshop` from the other host: the `peer agent` line reads `running`,
`stopped` (plist present, socket not answering), `not_installed` or
`unsupported` (not macOS). Use receipt-aware `provenant uninstall --harness NAME`
for each installed harness. Removing the last recorded target boots out the
agent and removes its unchanged plist; edited plists are preserved and
reported. See the [install runbook](install.md). Doctor uses a separate remote
command, so a key forced to the peer entrypoint cannot also run remote doctor.

`fabric peer` is a hidden machine entrypoint, not an interactive operator
command. The typed `PeerRequest`, `PeerAction` and `PeerResponse` in
`tools/front-door/crates/fabric-runtime/src/hosts.rs` own the wire format.
Requests bind source and target host IDs and a project locator. The transport
bounds frames to 1 MiB and refuses identity or version mismatches.

Tasks and messages have no `legacy` or `legacy_output_path` members. A store
or peer reply that still carries one, even as null, is refused.

Every request and reply envelope carries integer `protocol`, currently 5.
Ingress refuses a different protocol before decoding or executing the action,
replying with `peer_version` and its own version. The client checks the reply
protocol; a missing field or the released peer's decode refusal means protocol
1. Each call uses one SSH connection. A difference fails the call with
`peer_version` (`PEER_VERSION` on the CLI), for example: "peer workshop speaks
protocol 1, this host speaks 5; upgrade both". Peers must be upgraded together.
There is no capability negotiation or support for older peers. The framing
envelope's `version` remains 1.

Health replies also carry `protocol`. `hosts` retains a mismatched peer with
`available: false`, its remote protocol and `reason: "peer_version"`, alongside
the other hosts. Unprobed or unreachable peers omit `protocol`.

The released v1 peer-agent can hide a decode refusal in its log and close
without a reply. This ambiguous failure remains `home_unavailable` for reads
and `launch_unknown` for mutations, since it can also follow execution. Inspect
the peer-agent log and upgrade both hosts before retrying.

An older binary that already had the store open keeps working, but one that
opens it after the upgrade finds a newer schema and fails with `state_corrupt`.

Install the same release on both hosts and restart their peer agents, MCP
servers and running workers after upgrading. `not_terminal` and
`operation_conflict` travel unchanged over the peer wire; the CLI reports
`NONTERMINAL` and `OPERATION_CONFLICT`. An unavailable run owner reports
`OWNER_UNAVAILABLE`, distinct from an operation's `UNSUPPORTED` refusal.

## Dispatch and recovery

Use `provenant run` to submit one prompt, wait and read its result tail;
advanced `provenant dispatch` also accepts a batch manifest; `--host` names explicit placement on a configured peer, by label or
host ID. The peer resolves the route, confines the lane and runs it in its own
checkout of the same project, at the same path under its home directory, with
its own provider sign-ins; a `--account` pin is checked against the peer's registrations. The calling host keeps only a placement record under
`fabric/remote-runs/` in its state directory. `status`, `lanes --id`, `output`
and `cancel` use that record to find the peer by ID. `run` combines submission
with a bounded wait and result tail; its timeout prints the exact command to
continue waiting. Advanced `lanes --wait` and `lanes --watch` observe existing
peer runs. Lanes
keep running after the SSH call returns. Fabric MCP `fabric_run`
(`op: submit`) takes the same placement as an optional `host` field.
To read a peer's own attempt without a local placement record, use
`provenant output ATTEMPT --host workshop`, or `output HOST/ATTEMPT` with
the host ID. Output also accepts `RUN/ATTEMPT`; a local `--tail` above 500
is clamped to 500 lines.

A writer on a peer names a new branch and its base instead of `--worktree`,
which is a path on the calling host. This is the terminal path; agents use
`fabric_run` (`op: submit`):

```sh
provenant run --host workshop --mode worktree-write \
  --branch feat/x --base origin/main --route senior \
  --prompt-file prompt.md --deliver pr
```

The peer fetches every remote with its own Git credentials, then refuses with
`BASE_UNREACHABLE` unless a remote-tracking ref contains the base, so push the
base first. Use a remote-tracking ref or a pushed commit: a local branch name
resolves in the peer's clone. The peer creates the branch's registered
worktree with its own `worktree create`. A retry reuses that worktree while
it is clean at the base with no live writer; any other existing branch is
`BRANCH_EXISTS`. Delivery flags run host delivery on the peer with its Git and
`gh` credentials. Without them, the writer's commits stay in the peer's
worktree, and the dispatch receipt's `worktree` names its path and branch.
Replaying the operation returns the same run and worktree. One writer per
worktree, so a manifest cannot carry a peer writer. Status, wait, output and
cancel work as for read-only peer runs.

Naming this host (`local`, `localhost`, its host ID, or a peer label that resolves to it) is local
placement in any mode: the flag is dropped and the task runs here, where
`--branch` does not apply. A writer naming any other unconfigured host gets `UNKNOWN_HOST`.
Other limits on a peer run:

- explicit task IDs and route fallback are refused;
- `output` reads the result and stderr only, without `--tail`;
- `cancel` does not forward `--reason`; the peer records its own;
- `lanes` without `--id` lists only lanes on the calling host, and mixing
  local and peer runs in one call is refused. `lanes --host workshop` lists
  the peer's own lanes, newest first with `--all`. It takes `--state` and
  `--since` but not `--id` or `--cursor`, and filters only the page the peer
  returns. `lanes --host workshop --wait N` and `--watch` follow the peer's
  newest 100 lanes, counting only those that settle after the call starts. A
  lane seen running stays tracked until it settles, even after newer
  dispatches push it off that page. A queued or starting lane on the peer shows
  its `lane_class` and `waiting_for` like a local one, except in the final report
  of any `--wait` or `--watch` (including `dispatch` and `cancel`), which makes
  no read after its deadline; an older peer shows neither.

Waiting and watching work the same on both hosts. `lanes --wait N` returns when a
lane that was unsettled at the start, or settled since, meets `--condition`
(`any` by default). A settled older attempt of a named run does not count; if
every lane of the named runs settled earlier the call is refused with their
number, so pass the attempt ID, or `--since TIME` to count earlier
settlements. `--session LABEL` selects the lanes an agent label submitted, so
no ID list is needed. The wait starts when the command does, so a peer lane
that settles while the peer is unreachable still counts once it answers. The
comparison uses the peer's recorded settlement time against this host's clock,
so keep the hosts' clocks synchronised. For a background job or monitor, run
`provenant lanes --watch 3600`. One poll shares a single deadline across
all its peer reads (one per host and per 100 lanes, plus the lookup of lanes
that left the newest page). Each read is bounded by the time the earlier reads
left, as well as by `limits.peer_seconds`, and no read after the first starts
at or after the deadline; the poll then fails as a transient peer error, which
the wait retries until its own budget ends. The time a read has left covers
the whole SSH child, including its termination and reap, so a hung `ssh` that
ignores `TERM` is killed by the deadline. Only the first read of `--wait 0`
keeps a one-second minimum, with a further half second of termination and reap
if `ssh` hangs. The watch prints `STATE RUN/TASK/ATTEMPT` (and
`host=ID` for a peer's lane) once per state change, then `end settled lanes=N`,
or `end timeout lanes=N pending=M` with exit 124.

`provenant help run` shows all route, access, placement and retry controls.
CLI run, dispatch, cancel and resume generate and print an operation ID when
omitted; retain an explicit `--operation-id` for planned retries. The reserved observation
and control names are top-level `lanes`, `output`, `cancel`, `resume` and
`reconcile`. Targets follow the
grammar `RUN[/TASK[/ATTEMPT]]`, also accepting `RUN/ATTEMPT` and unique hex
prefixes of at least eight characters. An ambiguous prefix lists candidates.

The peer transport does not silently fall back to local placement. A mutation
whose reply is lost can return `launch_unknown`; retain its operation ID and
reconcile the recorded outcome before submitting replacement work. The host
owner also reports `peer_version`, `home_unavailable` and `state_corrupt` for
version, reachability and frame failures.

## Session hand-off

`provenant workflow session handoff` moves a chair or task session to a
peer. The peer starts a fresh Claude session from a hand-off prompt; no
transcript moves. The command sends one `session_handoff` peer request, so a
key restricted to `fabric peer` keeps working.

A hand-off opens an unconfined Claude session on the target, so the target
must opt in. It is off by default. On the host that will receive hand-offs,
run:

```sh
provenant config set fabric.accept_handoff true
```

The setting is host-wide: enabling it accepts hand-offs from every
configured peer, not one. Until then the host refuses with `HANDOFF_DISABLED`,
which the calling host reports as the error code; `provenant config unset
fabric.accept_handoff` turns it off again. The launch script prints the source
host and the prompt path before it starts `claude`. The calling host strips
control characters from, and bounds, every string in the peer's receipt, and
refuses a receipt whose worktree branches differ from the requested ones
(`HANDOFF_RECEIPT`).

```sh
provenant workflow session handoff --to workshop --account work \
  [--note TEXT]... [--attach PATH]... [--prompt-file PATH] [--no-checkpoint] \
  [--model ID] [--wip | --allow-dirty] [--dry-run] [--no-launch]
```

On the calling host the command:

1. reads the session checkpoint, unless `--no-checkpoint`. The session ID
   comes from `--session-id`, then `CLAUDE_CODE_SESSION_ID`, then
   `CLAUDE_SESSION_ID`;
2. builds the prompt and checks the whole request before it touches any tree.
   The prompt holds the checkpoint, each branch with its pushed commit, the
   lanes still running here and each `--note`. `--prompt-file` replaces the
   generated body; notes still follow it. Every prompt opens with
   `# Session hand-off`. The prompt is capped at 32 KiB, and a longer
   checkpoint is cut to fit with a note saying so. Attachments are capped at
   512 KiB in total;
3. checks that each registered worktree of the project is clean and pushed
   to `origin/<branch>`. Ignored files, such as build output, do not count as
   changes; they stay on the source and are not transferred. Files ignored
   only by a global excludes file still count. Without a flag it refuses with `WORKTREE_DIRTY` and lists the
   problem trees. `--wip` commits the tracked changes in each dirty or
   unpushed linked worktree as `WIP: session hand-off` and pushes it. It
   refuses the primary checkout, a detached tree, a tree with a live writer
   and untracked files Git does not ignore. If a push fails part way, the
   refusal names the branches already pushed. `--allow-dirty` proceeds and
   lists the problems in the prompt;
4. sends the request. `--dry-run` prints the prompt and plan and sends
   nothing. Once the peer accepts, the prompt is saved as the session
   checkpoint, so a refused or lost request leaves the checkpoint unchanged.

The peer checks `--account` against its registered Claude accounts and
refuses with `account_unregistered` when it does not match, without listing
the registered labels. It then claims
`handoff/<project-id>/op-<operation-id>/` in its state directory, fetches
`origin` and recreates each branch's worktree from `origin/<branch>`. An
existing worktree, or an existing local branch, is reused; when its commit
differs from origin or it has uncommitted changes other than ignored files,
the receipt and the prompt say so. The peer writes `prompt.md`, the attachments and
`launch.command` into the operation directory and runs
`open -a Terminal launch.command`. The script runs
`claude -- "$(cat prompt.md)"` with `CLAUDE_CONFIG_DIR` set to the account's
directory, so the prompt is never read as an option. `--no-launch` writes
everything, including the script, but opens no Terminal; run the script by
hand later. A failed launch is reported in the receipt and leaves the
delivered files in place.

Both hosts record the new owner in `handoff/<project-id>/owner.json` in their
state directories. The calling host moves its record only when Terminal
opened or `--no-launch` was given. The record is advisory: a later hand-off
from a host that is not the recorded owner warns but proceeds.

If the peer stores the hand-off but Terminal does not open, the command exits
non-zero with `LAUNCH_FAILED`, keeps the receipt in `data` and leaves both
leases unmoved; run the named `launch.command` on the peer by hand. Peer
refusals keep their stable codes (`PEER_VERSION`, `QUEUE_FULL`, `CONFLICT` and
the rest), the same as remote runs. The calling host sends its `limits.peer_seconds` and the peer works to the
smaller of that and its own, minus 5 seconds, so a slow worktree recreation is reported by the peer rather
than timing the calling host out.

Replaying an operation ID is refused with `already_delivered`, so a peer never
opens a second session for one hand-off. A lost reply returns
`LAUNCH_UNKNOWN`. Look for the operation's directory in the peer's state
directory before you retry, and remove it only if you want a second session.

Full chair hand-off, with the checkpoint and notes:

```sh
provenant workflow session handoff --to workshop --account work \
  --note "Watch CI on PR 1512, then merge." \
  --note "Lane 4 is still reviewing the hosts spec."
```

Quick task hand-off, with no checkpoint, after committing the tree as WIP:

```sh
provenant workflow session handoff --to workshop --account personal \
  --no-checkpoint --wip --note "Finish the failing parser test on feat/x."
```

Investigation from a written prompt with a log attached, while
this tree stays dirty:

```sh
provenant workflow session handoff --to workshop --account work \
  --no-checkpoint --allow-dirty --prompt-file investigate.md \
  --attach crash.log --dry-run
```

Drop `--dry-run` once the plan looks right. Follow the peer's lanes from here
with `provenant lanes --host workshop`.

## Legacy import data

A newer release cannot upgrade a store that still holds data from the removed
legacy import. Opening it fails with `upgrade_refused`; the message names the
table and its row count, and the store stays at a version the previous release
still opens. An import that brought nothing over does not block the upgrade.
When the refusing store is a project's home, a peer's events read receives
`upgrade_refused`: the home tries to pull events before opening coordination
and returns the refusal in an error frame. Other read-only calls report the
home as unavailable; changes report `launch_unknown`, and no receipt will
exist for them. For those failures, the reason and remedy are in the home's
`~/.local/share/provenant/v1/peer-agent/agent.log` while the peer agent runs.
With in-process handling, other read-only calls include the reason from SSH
stderr. Changes discard that captured stderr, so their callers see no reason.

Keep the previous release while you need that data. To move to the newer
release, archive the store and let Fabric create a fresh one:

1. On the previous release, record what you still need from
   `provenant fabric task list` and `provenant fabric mail read`, settle every
   unresolved landing push with `provenant fabric landing reconcile`, and
   resolve any peer operation that still reports `launch_unknown`, except calls
   refused before the store opened as described above: those have no receipt
   to reconcile. Active claims and landing leases do not survive the archive.
2. Stop every process that holds the store: MCP servers in open sessions, the
   `fabric peer-agent` LaunchAgent and running lanes.
3. Move the store files out of the state directory (here the default):

   ```sh
   state=~/.local/share/provenant/v1
   archive=~/fabric-store-archive-$(date +%Y%m%d)
   mkdir -m 700 "$archive"
   mv "$state"/fabric/fabric.sqlite3* "$archive"/
   ```

4. Run the newer release. It starts with no tasks, mail, events, claims or peer
   journal; create again any work you still need.

On a project's home host, the archive removes that project's tasks and mail for
every peer, and their in-flight operations against it report `launch_unknown`.
Archive the home only when its peers have no work in flight.

## Verify targeted changes

Run only fast tests for changed Rust boundaries. Full workspace suites run in
GitHub Actions. The owners are `provenant-fabric-runtime` and
`provenant-fabric-mcp`.
