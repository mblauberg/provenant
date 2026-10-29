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
general provider sign-in check. It also lists, for every registered account,
whether its managed skills, MCP entry and hooks match the installed release
(`provenant accounts sync` repairs drift; see the
[install runbook](install.md#account-sync)). Remote doctor uses
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
      "inputs": [{"node": "write", "outputs": ["result", "branch"]}],
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

Add a deterministic check after a review with a gate such as:

```json
{
  "id": "accept-review",
  "kind": "gate",
  "access": "read_only",
  "depends_on": ["review"],
  "inputs": [{"node": "review", "outputs": ["verdict"]}],
  "condition": {"type": "verdict", "expected": "VERDICT: PASS", "threshold": "all"}
}
```

Ask each reviewer to put its findings first and finish with exactly one
standalone verdict, for example `VERDICT: PASS` or `VERDICT: FAIL`. The token
must contain only uppercase ASCII letters, digits, underscores or hyphens;
the entire line must fit within 1024 bytes. Do not quote or repeat verdict
markers in prose. Multiple markers, malformed lines or a non-final verdict
fail the gate, even if other inputs satisfy its threshold. An incomplete
artifact fails the reviewer before the gate can consume it.

For threshold decisions, let reviewers report either verdict without a
`required_verdict` contract. A valid negative verdict then counts as a
non-match, so `any` or `majority` can tolerate it. A failed required-verdict
contract fails the reviewer and blocks its dependants before the gate can
evaluate; such contracts cannot express a tolerated negative review and do
not make ambiguous typed verdicts safe.
Thresholds are `all`, `any`, `majority` or `{"count": 2}`.
A gate has no route, prompt, writer worktree or
delivery configuration. Use `condition: {"type": "budget"}` without `inputs`
to check settled usage against the host's configured per-node and graph
budgets. This is a point-in-time check over settled attempts; it excludes
still-running branches and does not reserve future budget. Make the gate
depend on all nodes whose settled usage it should check before admitting the
work it guards. Reaching a budget or missing telemetry for a configured
metric fails that check; without a configured budget it passes.

Use a CI gate to wait for the checks on a pull request or branch:

```json
{
  "id": "ci",
  "kind": "gate",
  "access": "read_only",
  "condition": {"type": "ci", "target": "123", "checks": ["build", "test"], "timeout_seconds": 7200}
}
```

The `condition` fields for this gate are:

| Field | Meaning |
| --- | --- |
| `type` | `ci` |
| `target` | Non-empty pull request number or branch name, as a string |
| `checks` | Optional array of required check names |
| `timeout_seconds` | Optional timeout from 1 to 604800 seconds; defaults to 7200 |

Every explicitly named check must pass. When `checks` is omitted, the host
uses required checks from branch protection or rulesets if it can resolve them.
Otherwise every reported check must pass or be skipped, with at least one pass.
The gate records the policy it applied with its evidence. Failed checks fail
the gate. Missing checks, an unknown pull request or an API error fail with
reason `unavailable`. Pending checks leave the gate waiting until
they settle or the timeout expires. The checkpoint records the first observation,
last state and check summary. Pending gates are re-evaluated by `fabric graph run`
or `fabric graph resume` and when an attempt settles. Graph status reads the saved
snapshot; it does not advance the graph. No background timer re-checks the gate.

Set `on_fail` to a recovery node ID and include the gate in that node's
`depends_on`. The recovery path runs only on failure; the ordinary dependants
run only on success. Inactive paths become `skipped`. A failed gate without
recovery prevents delivery. Dependencies are conjunctive; a join that requires
both success and failure paths is rejected as unreachable. A successful
recovery permits final graph delivery; the usual budget admission fence still
applies to any recovery agent. Replay
retains a saved decision; use a new graph ID to evaluate it again.

Configure the `senior` routing pool and register `pro` with `provenant accounts
register` before running the graph. An alias-only route resolves its adapter
through the same routing configuration as `dispatch --route`. Explicit
adapter and model routes also work. A route must include an adapter or alias;
`route` selects one route, while `routes` provides an ordered fallback list
that can mix both forms. Both forms accept `account`. A node's optional `title`
appears in `lanes` and graph status; it defaults to the node ID and must pass
the task title validation, including the prohibition on control characters.

`inputs` selects outputs from direct dependencies to include in the node's
brief. The available selectors are `result`, `verdict`, `findings`, `branch`
and `commit`. Omit `inputs` for ordering without passing data. The result has a
SHA-256 digest of the complete stored result, a bounded credential-redacted
excerpt and a run-file path when available. The digest identifies the raw
stored artifact; hashing the redacted output may yield a different digest.
The path is available in status for a local attempt with a result; remote
attempts may have no local path. Paths are omitted from downstream briefs.
Selected outputs appear in a block marked as untrusted data. The rendered brief
is bounded by the effective `node_prompt_bytes` limit. If the prompt leaves no
room for the block or the selected data does not fit, the node fails before its
next attempt is admitted and reports an advance error. A failure before a
fallback preserves the previous attempt without creating a fallback attempt.
An admitted node without selected inputs replays its full prompt even if the
limit is lowered. Increase the limit in the host configuration and start a new graph to
retry a failed node. A document prompt over a structural limit instead pauses
the graph, which can be resumed after the limit is extended. Inspect node
outputs with `provenant fabric graph status --json`.

To combine outputs with an agent configured for synthesis, set
`"kind": "synthesise"`, give its prompt the purpose, and select at least one
dependency. For example:

```json
{
  "id": "synthesis", "kind": "synthesise", "access": "read_only",
  "prompt": "Deduplicate and rank the review findings.",
  "route": {"alias": "senior"}, "depends_on": ["review"],
  "inputs": [{"node": "review", "outputs": ["findings"]}]
}
```

Omitting `kind` or using `"task"` keeps task behaviour. All nodes require an
explicit `access`; gates require `"read_only"`.

### Plan a small graph

On a configured host with the default limits, save this as
`.provenant/scratch/graphs/plan-example.json`:

```json
{
  "schema_version": 1, "graph_id": "plan-example",
  "nodes": [{
    "id": "plan", "kind": "plan", "access": "read_only",
    "route": {"alias": "senior"},
    "plan": {"kinds": ["task"], "max_nodes": 2},
    "prompt": "Emit one read-only task with id inspect, goal Summarise README.md, write_scope [README.md], acceptance_checks [Identify the documented entry points], and depends_on []."
  }]
}
```

```sh
provenant fabric graph run --document .provenant/scratch/graphs/plan-example.json
provenant fabric graph status plan-example --json
```

The runtime supplies the planner's result contract. A complete result looks
like this; keep the JSON within 12 KiB and the final block within the retained
result tail:

````text
```fragment
{"schema_version":1,"nodes":[{"id":"inspect","goal":"Summarise README.md","access":"read_only","write_scope":["README.md"],"acceptance_checks":["Identify the documented entry points"],"depends_on":[]}]}
```
````

The task defaults to `senior`. Status shows `plan--inspect`, the validated
definitions under `checkpoint.expansions`, and `checkpoint.remaining`.
Dependencies and selected inputs in a fragment use local IDs. A planner may
declare `task`, `plan`, `synthesise` or `gate` children. Gates use the existing
condition schema. Presets and review shapes remain future extensions.
Planner prompts use the JSON kind names and require `worktree` for writers,
`inputs` for synthesis, and `acceptance_checks` for agent children. Keep the
planner ID, `--` and child ID within the 64-byte node ID limit.

For writers, create and register their worktrees first. Add
`"integration_branch": "integration"` to the graph and optionally
`"delivery": "pr"` for one final PR. Emitted writer nodes specify
`"access": "worktree_write"` and their registered `worktree`. The integration
branch must differ from the checked-out writer branch. Overlapping literal
file/directory scopes share the first writer's checkout and run serially with
`integrate:integration`; disjoint scopes in separate checkouts may run together.
Scopes use `.` for the whole repository and do not accept globs or `..`.
The admission fence also covers original graph writers; omitted scopes are
conservative and overlap all scopes. Skipped recovery writers do not constrain
later checkout selection. Plans inside loops must keep one writer checkout.

A writer that has committed a useful partial result can finish successfully
with this structured note instead of its completion verdict:

````text
```handoff
{"schema_version":1,"status":"stopped_early","note":"First half committed. Finish the second half, then run the acceptance checks."}
```
````

Use a note of at most 4096 bytes. The continuation keeps the checkout and
completion contract; consumers wait for it and read its selected outputs.
Use integrate delivery or no delivery for these writers. Graph checkouts are
retained for later work and explicit cleanup. Plain prose and failed provider
attempts do not trigger continuation.
Malformed optional handoffs are ignored with an output warning. The writer
still needs its configured completion verdict unless it emits a valid handoff.

The graph allows two continuations by default. Configure `continuations` in
the host graph policy; 0 disables them. To extend a paused graph, raise the
host limit and resume the graph.

Planner fragments refused for a structural limit are retained in the
checkpoint. Raise the named host limit to retry admission after reopening the graph. Invalid fragments still require
a corrected plan in a new graph; increasing a limit cannot repair their JSON.

Invalid planner output pauses with an explicit reason; correct the brief or
fragment configuration and start with a new graph ID. Raw fragment JSON is
retained for retry, but no invalid expansion is admitted. Status retains the
original handoff if a continuation cannot fit; it can resume after the relevant
limit is extended.

### Repeat review and repair

Use a `loop` node when review and repair should repeat until a gate passes.
For example, add this node to a graph after registering `.worktrees/feature`:

```json
{
  "id": "repair-review", "kind": "loop", "access": "read_only",
  "loop": {
    "max_rounds": 2,
    "until": "approved",
    "body": [
      {
        "id": "repair", "access": "worktree_write",
        "worktree": ".worktrees/feature", "route": {"alias": "senior"},
        "prompt": "Repair the change using previous review findings, if present. Commit it.",
        "inputs": [{"node": "previous.review", "outputs": ["result", "findings"]}]
      },
      {
        "id": "review", "access": "read_only", "route": {"alias": "senior"},
        "prompt": "Review .worktrees/feature. Report findings and finish with VERDICT: PASS or VERDICT: FAIL.",
        "depends_on": ["repair"],
        "inputs": [{"node": "repair", "outputs": ["result", "branch", "commit"]}]
      },
      {
        "id": "approved", "kind": "gate", "access": "read_only",
        "depends_on": ["review"],
        "inputs": [{"node": "review", "outputs": ["verdict"]}],
        "condition": {"type": "verdict", "expected": "VERDICT: PASS", "threshold": "all"}
      }
    ]
  }
}
```

Round one omits `previous.review`; round two receives round one's result and
findings. Current-round references use the local node ID, such as `repair`.
All writers in the body must use the same worktree, so repairs continue on the
existing branch. The `until` gate must follow the entire body. Body delivery,
`on_fail` and nested loops are refused. A task failure fails the loop; a
negative `until` verdict starts the next round.

Configure host limits for the full expansion before running. This example
requires seven nodes and a dependency depth of six. The default host depth
limit is 64, which covers two rounds of the bundled `big-batch` review loop. Composite nesting counts against the
same limit. To allow many more rounds, raise it:

```toml
[domains.fabric.graph.limits]
expansion_depth = 64
```

Preset and graph `read_roots` must lie inside the project or its linked
worktrees. List any other directory by absolute path:

```toml
[domains.fabric.graph]
allowed_read_roots = ["/absolute/path/to/shared-reference"]
```

All allowed rounds count
towards structural and prompt limits even if the first round passes. An
extension adds to that expansion and must fit host limits. The safety bounds
are 1024 rounds, 16384 expanded nodes and 64 bytes per expanded node ID.

Inspect rounds with `provenant fabric graph status GRAPH_ID --json`. Body IDs
include their round, for example `repair-review--r2--review`; each checkpoint
node includes `round` metadata and its typed outputs. Passing the gate skips
future rounds and lets dependants of `repair-review` continue. After two
negative rounds the graph reports `paused` with an exhaustion reason.

To allow two more rounds, keep the graph document unchanged and set the
total allowance to four. The extension must fit the host limits:

```sh
provenant fabric graph resume --document graph.json \
  --extend loop.repair-review.rounds=4
```

Loop extensions require an exhausted loop. `--extend loop.repair-review.rounds=4`
sets the absolute round allowance. `--extend loop.repair-review.decision=stop`
fails the loop, while `--extend loop.repair-review.decision=accept` accepts its
current outputs and continues. The runtime checks the expanded graph against
host limits before saving an extension. A loop can also set
`max_extension_rounds` (default five) to cap an inbox extension; the total
rounds remain at most 1024.

### Decide nodes and graph asks

Graphs use one checkpointed ask protocol for human decisions. Add a read-only
`decide` node to ask a chair to choose an option, then route its verdict through
a verdict gate:

```json
{
  "schema_version": 1,
  "graph_id": "choose-repair",
  "decision_session": "CHAIR_SESSION_ID",
  "nodes": [
    {
      "id": "choice",
      "kind": "decide",
      "access": "read_only",
      "decide": {
        "question": "Which repair should proceed?",
        "options": [
          {"id": "repair", "label": "Repair"},
          {"id": "defer", "label": "Defer"}
        ]
      }
    },
    {
      "id": "route-choice",
      "kind": "gate",
      "access": "read_only",
      "depends_on": ["choice"],
      "inputs": [{"node": "choice", "outputs": ["verdict"]}],
      "condition": {"type": "verdict", "expected": "repair", "threshold": "all"},
      "on_fail": "defer"
    },
    {
      "id": "defer",
      "kind": "task",
      "access": "read_only",
      "prompt": "Record that this work is deferred.",
      "route": {"alias": "senior"}
    }
  ]
}
```

Use the graph's `decision_session` or set `session` on the decide node. A decide
node may also set a `default` option and positive `timeout_seconds`; the timeout
requires a default and the graph takes that option at the saved deadline. A
chosen option ID becomes the node's verdict. An answer that does not name an
option is rejected and the graph asks again with the reason. Decide nodes run
without an attempt and do not use graph concurrency.

The same ask primitive handles exhausted loops and graph pauses. A loop may set
`decision_session` in its `loop` object; its ask offers stop, extend or accept.
An extension can add one round, or a JSON reply can add a positive number up to
`max_extension_rounds` and host limits. A graph-level `decision_session` also
lets a budget or structural pause ask to resume, cancel or apply budget
extensions. To extend a budget in the inbox, reply with
`{"decision":"extend","extensions":["budget.per_graph.tokens=2000000"]}`.
Asks are keyed by ID in the checkpoint, so several can be open and retries
reuse the same request. Answer through the inbox, then run
`provenant fabric graph resume --document graph.json` to advance the checkpoint.
See [graph asks in the inbox runbook](human-inbox.md#exhausted-graph-loops) for
answer shapes and replay behaviour. Without a configured decision session, use
the explicit resume extensions above.

### Inspect findings and recover a graph

To report findings, include a fenced `findings` block in the result containing
JSON with `schema_version: 1` and a `findings` array. Each finding has `id`,
`severity`, `file`, `line`, `summary` and `status`. For example:

````text
```findings
{"schema_version":1,"findings":[{"id":"F1","severity":"high","file":"src/lib.rs","line":12,"summary":"Missing validation","status":"open"}]}
```
````

The runtime accepts at most 32 findings. A missing or invalid block produces
an empty list and, when a result exists, a warning rather than failing the node. See the
[graph spec](../specs/fabric.md#typed-node-outputs) for the output contract.

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

Tasks and messages no longer use `legacy` or `legacy_output_path` members.
Stored and peer records tolerate additional fields. Records included in a
canonical digest retain those fields when read and written again. MCP tool
arguments and user-authored configuration still reject unknown fields.

Every request and reply envelope carries integer `protocol`, currently 6.
Ingress refuses a different protocol before decoding or executing the action,
replying with `peer_version` and its own version. The client checks the reply
protocol; a missing field or the released peer's decode refusal means protocol
1. Each call uses one SSH connection. A difference fails the call with
`peer_version` (`PEER_VERSION` on the CLI), for example: "peer workshop speaks
protocol 1, this host speaks 6; upgrade both". Peers must be upgraded together.
There is no capability negotiation or support for older peers. The framing
envelope's `version` remains 1.

Health replies also carry `protocol`. `hosts` retains a mismatched peer with
`available: false`, its remote protocol and `reason: "peer_version"`, alongside
the other hosts. Unprobed or unreachable peers omit `protocol`.

The released v1 peer-agent can hide a decode refusal in its log and close
without a reply. This ambiguous failure remains `home_unavailable` for reads
and `launch_unknown` for mutations, since it can also follow execution. Inspect
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
the host ID. Output also accepts `RUN/ATTEMPT`. The default human result is a
settlement line and the whole final result up to 12 KiB (head and tail above
that, naming the omitted bytes); `--full` reads the full byte window and
`--json` retains the machine envelope. Changed files use the recorded base and final head where
their objects remain readable; peer summaries report this evidence as
unavailable rather than guessing a diff.

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
- `output` reads result, prompt, stderr and events; transcripts and receipts
  require running the command on the peer host;
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
the wait retries until its own budget ends.
Local run-store contention is also retried between polls until the wait's
deadline. Each local store operation makes an immediate attempt without
waiting on contention, so a successful poll still observes settlement;
persistent contention ends the wait or watch as a timeout. Optional wait
details in the final reply use the same nonblocking reads. If peer reads
remain unavailable at the deadline, the last failure is reported.
Run-store privacy errors identify the path relative to the state root and the
failed check; `.` denotes the state root itself. Paths outside that root, such
as worktree paths, are reported as given.
The time a peer read has left covers
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

Migration 007 deliberately deletes records from the removed legacy import,
including legacy tasks and messages with `scope: legacy`, rather than refusing
the upgrade or leaving unreadable rows. Archive a store with the previous
release before upgrading if you still need its imported history. Records
written after migration retain unknown fields, including retired member names.

## Verify targeted changes

Run only fast tests for changed Rust boundaries. Full workspace suites run in
GitHub Actions. The owners are `provenant-fabric-runtime` and
`provenant-fabric-mcp`.
