# Fabric hosts
Owner: provenant-fabric-runtime
Anchor: [`hosts.rs`](../../tools/front-door/crates/fabric-runtime/src/hosts.rs); [`peer_agent.rs`](../../tools/front-door/crates/fabric-runtime/src/peer_agent.rs)

Setup for a second host. Fabric uses bounded SSH calls and host-owned state;
each host keeps its own provider sign-ins. The peer model and boundaries are in
the [Fabric spec](../specs/fabric.md#peer-hosts); running work is
`provenant run`, `status`, `output`, `retry` and `stop` as on one host.

## Configure hosts

Install the same release on both hosts, reachable by non-interactive SSH, and
run `provenant fabric identity` on each to get its host ID. In each host's
state-root `config.toml` (`--state-dir` or `PROVENANT_STATE_DIR`; check with
`provenant config show`) name the other:

```toml
[domains.fabric.peers.workshop]
host = "<peer host ID from fabric identity>"
destination = "workshop"                     # SSH alias or user@alias, no options
remote_executable = "/opt/provenant/bin/provenant"   # optional, absolute
connect_timeout_seconds = 10                 # 1 to 30
```

`domains.fabric.ssh_executable` overrides SSH discovery. A key restricted to
the peer entrypoint works:
`command="/absolute/path/to/provenant fabric peer",restrict ssh-ed25519 KEY user`.
`provenant config schema` lists the other `domains.fabric` keys.

`provenant hosts doctor [NAME]` probes Claude identities under `~/.claude` and
`~/.claude-*` and reports whether managed skills, MCP entry and hooks match the
release (`provenant accounts sync` repairs drift). With `NAME` it runs the same
read-only probe on the peer; an unknown name is `UNKNOWN_HOST`.

Peers must be upgraded together: every request carries a protocol number and a
mismatch fails with `PEER_VERSION` (for example "peer workshop speaks protocol
1, this host speaks 6"). Upgrade each host with `provenant upgrade`, then
restart peer agents, MCP servers and workers. A peer too old or unable to
submit is refused before launch.

## macOS peers and the login keychain

A process started from SSH on macOS cannot read the login keychain, so
`claude auth status` reports no login and Claude routes fail with a
login-required refusal (Codex keeps its credential in a file and is
unaffected). `provenant install` for the account's own home and the default
state root therefore writes and starts the LaunchAgent
`~/Library/LaunchAgents/dev.provenant.peer.plist`, which runs each peer call in
the login session over `~/.local/share/provenant/v1/peer-agent/peer.sock`. When
the agent is not listening, `fabric peer` handles the call itself.

- Re-running install is idempotent and restarts the agent on the new binary.
  The plist's PATH keeps only fixed system directories, the install bin
  directory and directories holding the Claude, Codex and Git executables found
  when install ran: run it from a shell whose PATH finds them. A different home
  or `--state-dir` skips the agent.
- `hosts doctor` shows the `peer agent` line as `running`, `stopped` (plist
  present, socket not answering), `not_installed` or `unsupported`. Failed
  requests are logged in `peer-agent/agent.log`.
- A lost reply to a mutation is `LAUNCH_UNKNOWN`: keep the operation ID and
  reconcile before submitting replacement work. The transport never falls back
  to local placement.
- `provenant uninstall --harness NAME` boots out the agent with the last
  recorded target; an edited plist is preserved and reported.

Committed code moves between hosts with `git fetch`, never a bundle; remove
finished `refs/provenant/transport/<operation>/export` refs with `git update-ref -d`.

Peer writers use work bindings for placement, branch, base and write scope.
Push the base first: the peer resolves it in its own clone with its own Git
credentials. `output --tail` and receipts over peers are bounded; receipts are
not supported over peer transport, so run them on the peer host.

## Session hand-off

`provenant workflow session handoff --to PEER --account LABEL` starts a fresh
Claude session on a peer from a hand-off prompt (no transcript moves). It opens
an unconfined session there, so the receiving host must opt in, host-wide and
for every configured peer:

```sh
provenant config set fabric.accept_handoff true    # unset to turn off
```

Until then the peer refuses with `HANDOFF_DISABLED`. Flags (`--help`): `--note`,
`--attach`, `--prompt-file`, `--no-checkpoint`, `--model`, `--dry-run`,
`--no-launch` and `--session-id` (else `CLAUDE_CODE_SESSION_ID`).

- Every registered worktree must be clean and pushed to `origin/BRANCH`, or the
  command refuses with `WORKTREE_DIRTY`. `--wip` commits tracked changes in each
  linked worktree as `WIP: session hand-off` and pushes (refusing the primary
  checkout, a detached tree, a live writer and unignored untracked files);
  `--allow-dirty` proceeds and lists the problems in the prompt. Start with
  `--dry-run`.
- The prompt is capped at 32 KiB and attachments at 512 KiB. The peer recreates
  each branch's worktree from `origin/BRANCH` under `handoff/<project-id>/op-<id>/`
  and opens Terminal on `launch.command` (run it by hand with `--no-launch`).
  `LAUNCH_FAILED` means the files arrived but Terminal did not open.
- Replaying an operation ID is refused with `already_delivered`; after
  `LAUNCH_UNKNOWN` look for the operation's directory on the peer before
  retrying, and remove it only if you want a second session.
