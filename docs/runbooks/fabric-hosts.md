# Fabric hosts

Status: current Rust operator surface

Fabric uses bounded SSH calls and host-owned state. Each host keeps its own
provider sign-ins. The [accepted specification](../specs/fabric-hosts.md)
records the original requirements; the [Rust Fabric design](../specs/rust-port/design/fabric.md)
owns the replacement boundaries.

## Commands

```sh
provenant help
provenant fabric host list
provenant hosts doctor
provenant fabric peer
provenant fabric peer-agent
provenant dispatch --host workshop --mode read-only --help
```

`fabric host list` reads local and configured peer health. `hosts doctor`
probes only Claude identities under `~/.claude` and `~/.claude-*`; it is not a
general provider sign-in check. Remote doctor uses
`~/.local/bin/provenant` on the peer. Peer calls run `provenant fabric peer`
through the peer's non-interactive `PATH`. The hidden `fabric peer` command is
the fixed SSH protocol entrypoint; the hidden `fabric peer-agent` command is the
macOS LaunchAgent that runs it in the login session (see below). Remote dispatch uses top-level
`dispatch --host`. The Rust CLI grammar lives in
`tools/front-door/crates/cli/src/grammar.rs` and
`tools/front-door/crates/cli/src/fabric.rs`.

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
and restarts the agent so it serves the new binary. The plist captures the
installer's `PATH`, so run install from a shell whose `PATH` finds `claude`
and the other provider CLIs. Installs with another home or `--state-dir` skip
the agent. The agent logs failed requests to
`~/.local/share/provenant/v1/peer-agent/agent.log`.

Check it with `provenant hosts doctor` locally or `provenant hosts doctor
workshop` from the other host: the `peer agent` line reads `running`,
`stopped` (plist present, socket not answering), `not_installed` or
`unsupported` (not macOS). To remove it, run
`launchctl bootout gui/$(id -u)/dev.provenant.peer` and delete the plist. Doctor uses a separate remote command, so a key forced to the peer
entrypoint cannot also run remote doctor.

`fabric peer` is a hidden machine entrypoint, not an interactive operator
command. The typed `PeerRequest`, `PeerAction` and `PeerResponse` in
`tools/front-door/crates/fabric-runtime/src/hosts.rs` own the wire format.
Requests bind source and target host IDs and a project locator. The transport
bounds frames to 1 MiB and refuses identity or version mismatches. Do not send
the retired `{protocol_version, verb, params}` envelope.

## Dispatch and recovery

The CLI grammar uses `provenant dispatch` for one prompt or a batch
manifest; `--host` names explicit placement on a configured peer, by label or
host ID. The peer resolves the route, confines the lane and runs it in its own
checkout of the same project, at the same path under its home directory, with
its own provider sign-ins. The calling host keeps only a placement record under
`fabric/remote-runs/` in its state directory. `lanes --id`, `output` and `cancel` find that record and forward to
the peer by ID, and `dispatch --wait` and `lanes --wait` poll the peer. Lanes
keep running after the SSH call returns. The Fabric MCP `fabric_submit` tool
takes the same placement as an optional `host` field.

A writer on a peer names a new branch and its base instead of `--worktree`,
which is a path on the calling host:

```sh
provenant dispatch --host workshop --mode worktree-write \
  --branch feat/x --base origin/main --model ADAPTER/ID \
  --operation-id OP --prompt-file prompt.md \
  [--deliver-remote origin --deliver-branch feat/x [--open-pr]]
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
  local and peer runs in one call is refused.

`provenant help dispatch`
shows the operation ID, route and access controls. The reserved observation
and control names are top-level `lanes`, `output`, `cancel`, `resume` and
`reconcile`. Targets follow the
Rust grammar `RUN[/TASK[/ATTEMPT]]`; do not reuse the retired `id@host`
selectors or `fabric fetch` workflow.

The peer transport does not silently fall back to local placement. A mutation
whose reply is lost can return `launch_unknown`; retain its operation ID and
reconcile the recorded outcome before submitting replacement work. The host
owner also reports `peer_version`, `home_unavailable` and `state_corrupt` for
version, reachability and frame failures.

## Verify targeted changes

Run only fast tests for changed Rust boundaries. Full workspace suites run in
GitHub Actions. The current owners are `provenant-fabric-runtime` and
`provenant-fabric-mcp`; the Python and TypeScript host owners are retired.
