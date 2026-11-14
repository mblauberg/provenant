# Fabric hosts
Owner: provenant-fabric-runtime
Anchor: [hosts.rs](../../tools/front-door/crates/fabric-runtime/src/hosts.rs); [`peer_agent.rs`](../../tools/front-door/crates/fabric-runtime/src/peer_agent.rs)

Setup for a second host. Fabric uses bounded SSH calls and host-owned state;
each host keeps its own provider sign-ins. The peer model and boundaries are in
the [Fabric spec](../specs/fabric.md#peer-hosts); running work is
`provenant run`, `status`, `output`, `retry` and `stop` as on one host.

## Configure hosts

Install the same release on both hosts, reachable by non-interactive SSH, and
run `provenant fabric identity` on each to get its host ID. In each host's
state-root `<state>/config.toml` (`--state-dir` or `PROVENANT_STATE_DIR`; check with
`provenant config show`) name the other:

```toml
[domains.fabric.peers.workshop]
host = "<peer host ID from fabric identity>"
destination = "workshop"                     # SSH alias or user@alias, no options
remote_executable = "/opt/provenant/bin/provenant"   # optional, absolute
connect_timeout_seconds = 10
```

`domains.fabric.ssh_executable` overrides SSH discovery. A key restricted to
the peer entrypoint works:
`command="/absolute/path/to/provenant fabric peer",restrict ssh-ed25519 KEY user`.
`provenant config schema` lists the other `domains.fabric` keys.

`provenant hosts doctor [NAME]` probes Claude identities under `~/.claude` and
`~/.claude-*` and reports whether managed skills and hooks match the
release (`provenant accounts sync` repairs drift). With `NAME` it runs the same
read-only probe on the peer; an unknown name is `UNKNOWN_HOST`.

If a peer reports a protocol mismatch, upgrade the hosts together using the
[upgrade procedure](install.md#upgrade), then restart peer agents and workers.

## macOS peers and the login keychain

A peer call started over SSH on macOS cannot read the login keychain. Install
in the account's own home and default state root to use a peer agent in the
login session. Run install with a PATH that finds the provider and Git CLIs.
See [background agents](install.md#background-agents) for lifecycle and repair.
Use `provenant hosts doctor` to check the agent; inspect its log if requests fail.

If a mutation loses its reply, keep its operation ID and reconcile before
submitting replacement work.

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

Start with `provenant workflow session handoff --help` and a dry run. Clean and
push each registered worktree before handing off. If a launch fails, check the
peer for the delivered files and launch the session there. If the reply is
lost, inspect the peer for the operation before retrying to avoid a duplicate
session.
