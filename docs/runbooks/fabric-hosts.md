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
provenant status --all
provenant workflow session handoff --help
```

`hosts` reads local and configured peer health. `hosts doctor`
probes only Claude identities under `~/.claude` and `~/.claude-*`; it is not a
general provider sign-in check. It also lists, for every registered account,
whether its managed skills, MCP entry and hooks match the installed release
(`provenant accounts sync` repairs drift; see the
[install runbook](install.md#account-sync)). Remote doctor names a configured
peer: `hosts doctor NAME` resolves `NAME` through `domains.fabric.peers` and
sends a read-only doctor action over the same `provenant fabric peer` transport
as the other peer reads. It uses the peer's `destination`,
`domains.fabric.ssh_executable` and `remote_executable`, so a key restricted to
the peer entrypoint still serves it. An unknown name is refused as
`UNKNOWN_HOST`. Remote doctor requires a project checkout with Fabric peer
configuration; the peer must hold the same project key. Peer calls run
`provenant fabric peer` through the peer's
non-interactive `PATH`. The hidden `fabric peer` command is the fixed SSH
protocol entrypoint; the hidden `fabric peer-agent` command is the macOS
LaunchAgent that runs it in the login session (see below). Remote dispatch uses
work bindings in template documents. The Rust CLI grammar lives in
`tools/front-door/crates/cli/src/grammar.rs` and
`tools/front-door/crates/cli/src/fabric.rs`.

## Local graphs

Use the [template runbook](presets.md) for graph composition, review panels,
repair loops and decisions. The public document is a collection of template
nodes; compiled graph kinds are internal.

```sh
provenant run --route junior --file brief.md --explain
provenant run --use plan --file brief.md
provenant status GRAPH
provenant output GRAPH --trace
provenant retry GRAPH
provenant stop GRAPH
```

Inspect the explanation before starting work. `run` inside a lane adds children
to the same graph. Children share budget, stopping and trace. Use `edit GRAPH
--file PATCH` to change unstarted nodes; started attempts remain immutable.
For a paused decision, use the [human inbox](human-inbox.md), then retry the
existing graph. Preserve its checkpoint and completed outputs during recovery.

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
remote_executable = "/Users/alice/.cargo/bin/provenant" # optional
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

[domains.fabric.graph.budget]
unknown_usage = "warn" # or "pause"

[domains.fabric.graph.budget.per_node]
tokens = 100000
cost_usd = 5.0
wall_time_seconds = 1800

[domains.fabric.graph.budget.per_graph]
tokens = 1000000
cost_usd = 50.0
wall_time_seconds = 7200

[domains.fabric.graph.land]
routine = "auto"     # default
substantial = "auto" # default
crucial = "ask"      # default
terminal = "ask"     # default

[domains.fabric.graph.land.projects."PROJECT_ID"]
substantial = "ask"
```

Limits also include `routes_per_node`, `node_prompt_bytes`,
`graph_prompt_bytes`, `expansion_depth`, `fan_out_width`, `continuations` and `concurrency`.
`review_substitutions` bounds retries after settled lens and verifier task failures
and verified preparation refusals;
its default is 1, and 0 disables those retries.
The defaults are 16 routes, 256 KiB per node prompt, 4 MiB per graph,
expansion depth 64 and fan-out 32. Set concurrency to 0 to use host lane caps.
Fan-out counts direct dependants of a node; expansion depth is the longest
dependency chain in edges, with root nodes at depth 0.
Budget fields are optional; unset fields have no budget. Costs are in US dollars.
Document `limits` are ignored with a warning; host limits always apply.
When a structural limit or budget is reached, graph status reports `paused` and
the checkpoint records the reason. A paused command exits successfully; scripts
should inspect the status. Running attempts can finish. Raise the host limit and
resume the graph, or extend a configured budget with `--extend per_graph.tokens=2000000`.
Budget extensions are recorded in the checkpoint and use absolute values.
Token counts and estimated
cost use attempt telemetry. Missing or partial usage is reported as unknown
in graph status and the checkpoint's usage events. By default,
`unknown_usage = "warn"` continues with a visible warning; set it to `"pause"`
to pause instead. Wall time uses
the sum of observed provider run times, excluding time spent paused or idle,
and is checked on graph advancement and attempt settlement.

`graph.land` sets what a graph land node does for each risk tier: `auto`
merges once the gates pass, and `ask` raises a land ask in the graph's
decision session first. A tier set under `projects."PROJECT_ID"` overrides
the host value for that project's graphs, and unset tiers fall through to the
host value and then the default. Project keys are the project's ID. The land
record in the checkpoint names the effective policy, its source (`default`,
`config` or `project`) and who or what authorised the merge. Graph land uses
the same merge path as `provenant land`, so the repository's merge rules and
CI still apply.

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
error and, for a mutation, as `LAUNCH_UNKNOWN`. The agent runs at most 64
requests concurrently. Excess connections close immediately and surface as
the peer agent's stable empty-reply error.

Committed code moves between hosts with `git fetch`, never a bundle file.
The sending checkout must be clean, with the head as its `HEAD`; it binds
`refs/provenant/transport/<operation>/export`. The receiver fetches that ref
into `refs/provenant/transport/<operation>/incoming` with `fetch.fsckObjects`,
`transfer.fsckObjects` and `unpackLimit=1`, so `index-pack --strict` checks
every object and connectivity before the ref moves. It then promotes
`<operation>/verified-head` once the OID and ancestry match. A failed fetch can
install valid objects before a later ref update fails. OID or ancestry
rejection leaves the incoming ref in place. Remove finished transport refs
with `git update-ref -d`; `git gc` reclaims unreachable objects. There is no
size limit; a fetch may run for 30 minutes.

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
reported. See the [install runbook](install.md). Remote doctor uses the same
forced-command peer entrypoint, so a key restricted to `provenant fabric peer`
also serves it.

`fabric peer` is a hidden machine entrypoint, not an interactive operator
command. The typed `PeerRequest`, `PeerAction` and `PeerResponse` in
`tools/front-door/crates/fabric-runtime/src/hosts.rs` own the wire format.
Requests bind source and target host IDs and a project locator. The transport
bounds frames to 1 MiB and refuses identity or version mismatches.

Tasks and messages no longer use `legacy` or `legacy_output_path` members.
Stored and peer records tolerate additional fields. Records included in a
canonical digest retain those fields when read and written again. MCP tool
arguments remain strict. User configuration warns on unknown keys and invalid
optional input; `provenant config check` lists the source file and key.

Every request and reply envelope carries integer `protocol`, currently 6.
Ingress refuses a different protocol before decoding or executing the action,
replying with `peer_version` and its own version. The client checks the reply
protocol; a missing field or the released peer's decode refusal means protocol
1. Each exchange uses one SSH connection. A difference fails the call with
`peer_version` (`PEER_VERSION` on the CLI), for example: "peer workshop speaks
protocol 1, this host speaks 6; upgrade both". Peers must be upgraded together.
Before a submission, the chair reads the peer health response for its release
and run capabilities in a separate, short exchange. A peer reporting a release
older than 1.6.5 or reporting that it cannot submit is refused before launch;
upgrade an older peer with `provenant upgrade` on that host. Missing or
unparseable advertisements and failed probes do not prevent submission; the
peer can still refuse an incompatible protocol. The framing envelope's `version` remains 1.

Health replies also carry `protocol`. `hosts` retains a mismatched peer with
`available: false`, its remote protocol and `reason: "peer_version"`, alongside
the other hosts. Unprobed or unreachable peers omit `protocol`.

The released v1 peer-agent can hide a decode refusal in its log and close
without a reply. This ambiguous failure remains `home_unavailable` for reads
and `LAUNCH_UNKNOWN` for mutations, since it can also follow execution. Inspect
the peer-agent log and upgrade both hosts before retrying.

Readers in this release report `upgrade_required` when a stored record,
graph checkpoint, graph document or database uses a newer schema. The error
names the running reader release and the schema version. The writer release
is `unknown` where the record does not store it. A stale binary predating this
handling may still report `state_corrupt`; restart it after upgrading.

Install the same release on both hosts and restart their peer agents, MCP
servers and running workers after upgrading. `not_terminal` and
`operation_conflict` travel unchanged over the peer wire; the CLI reports
`NONTERMINAL` and `OPERATION_CONFLICT`. An unavailable run owner reports
`OWNER_UNAVAILABLE`, distinct from an operation's `UNSUPPORTED` refusal.

## Dispatch and recovery

Use `provenant run --use task --file BRIEF` for one assignment, or a composite
such as `batch` for disjoint implementation slices. Inspect with `--explain`.
MCP `fabric_run` calls the same host service; documents carry placement and
work bindings. Host-specific routing and sign-in remain on the executing peer.

Use `status GRAPH` to inspect progress, `output TARGET` to collect evidence,
`stop TARGET` to stop a subtree and `retry TARGET` to recover unfinished work.
`status --wait=5m --target graph:GRAPH` observes a bounded wait; expiry leaves
work running. Checkpoints preserve completed results and uncertain effects
must be reconciled before retrying. Use `output TARGET --trace` for lineage.

`--tail N` works locally and on peers, is clamped to 500 lines and retains at
most `--max-bytes`. Peer tails page within a shared 30-second budget and
refuse a changing artifact. Event tails retain whole records; local receipt
tails return the requested provenance lines, including `tail_text` in JSON.
For a settled local attempt, `output RUN --part receipt`
prints its provenance when the run has exactly one attempt. Read-only tasks
use `Reviewer:` and writer tasks use `Writer:`; JSON includes the matching `role`.
Missing route, effort, catalogue family or resolution evidence is `unknown`, never
a guessed default. Settled attempts without a recorded route still return a receipt.
A second `Reviewer provenance:` line reports the canonical reviewer's provenance,
actor, observed model and catalogue family; missing fields are `unknown`. A
`RUN/TASK` target likewise must select exactly one attempt; otherwise use
`RUN/TASK/ATTEMPT`. Add `--json` for the individual fields. Receipt output
is not supported over peer transport; run the command on the peer host.

Peer writers use document work bindings for placement, branch, base and write
scope. Push the base first: the peer resolves it in its own clone and uses its
own Git credentials. Keep one writer per worktree and collect the recorded
branch, commit and route before integrating.

Observe from the owning host with `status TARGET` and collect with `output
TARGET`. For a monitor, use `status --wait=1h --target graph:GRAPH --follow`.
The deadline bounds observation and leaves active work running. Reconcile
unavailable or uncertain custody before retrying. `help --all` lists advanced
host controls; `help run` describes the template front door.

The peer transport does not silently fall back to local placement. A mutation
whose reply is lost can return `LAUNCH_UNKNOWN`; retain its operation ID and
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
with `provenant status --all` on that host.

## Legacy import data

Migration 007 deliberately deletes records from the removed legacy import,
including legacy tasks and messages with `scope: legacy`, rather than refusing
the upgrade or leaving unreadable rows. Archive a store with the previous
release before upgrading if you still need its imported history. Records
written after migration retain unknown fields, including retired member names.

## Verify targeted changes

Run only fast tests for changed Rust boundaries. Full workspace suites run in
GitHub Actions. The owners are `provenant-fabric-runtime` and
`provenant-fabric-mcp`.
