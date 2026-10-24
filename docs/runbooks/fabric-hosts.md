# Fabric hosts

Status: current Rust operator surface

Fabric uses bounded SSH calls and host-owned state. Each host keeps its own
provider sign-ins. The [accepted specification](../specs/fabric-hosts.md)
records the original requirements; the [Rust Fabric design](../specs/rust-port/design/fabric.md)
owns the replacement boundaries.

## Commands

```sh
provenant help
provenant hosts list
provenant hosts doctor
provenant peer
provenant fabric dispatch --help
```

`hosts list` reads local and configured peer health. `hosts doctor` probes
provider sign-in; optional host selectors target configured peers. The hidden
top-level `peer` command is the fixed SSH protocol entrypoint. Fabric dispatch
is `provenant fabric dispatch`. The Rust CLI grammar lives in
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

[domains.fabric.peers.workshop]
host = "<peer host ID from fabric identity>"
destination = "workshop"
```

Replace the placeholder with the peer's actual host ID. Configure the reverse
peer on the other host. `home_hosts` maps cross-host project keys to home host
IDs. Peer destinations are bounded SSH aliases or `user@alias`, with no shell
options. `domains.fabric.limits.peer_seconds` bounds a call to at most 30
seconds. The complete field validation and compiled defaults live in
`tools/front-door/crates/fabric-runtime/src/config.rs` and its `assets/`.

## SSH and peer protocol

Configure SSH aliases and keys in the user's OpenSSH configuration. The Rust
transport discovers `/usr/bin/ssh` or `/bin/ssh` and sends the fixed remote
command `provenant peer`. It accepts no configurable peer command or
SSH-program environment override. A restricted key can use:

```text
command="provenant peer",restrict ssh-ed25519 <public-key> user
```

Verify that the receiving host can resolve the binary in this non-interactive
context. Doctor uses a separate remote command, so a key forced to the peer
entrypoint cannot also run remote doctor.

`fabric peer` is a hidden machine entrypoint, not an interactive operator
command. The typed `PeerRequest`, `PeerAction` and `PeerResponse` in
`tools/front-door/crates/fabric-runtime/src/hosts.rs` own the wire format.
Requests bind source and target host IDs and a project locator. The transport
bounds frames to 1 MiB and refuses identity or version mismatches. Do not send
the retired `{protocol_version, verb, params}` envelope.

## Dispatch and recovery

The CLI grammar reserves `provenant dispatch` for one prompt or a batch
manifest; `--host` names explicit placement. `provenant help dispatch`
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
