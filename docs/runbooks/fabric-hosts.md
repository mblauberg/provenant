# Fabric hosts

Status: current
Applies to: `provenant hosts`, `provenant peer` and `fabric_hosts`

Host diagnostics work over the user's OpenSSH configuration. Each host keeps
its own instance state and provider sign-ins. The [accepted
specification](../specs/fabric-hosts.md) and [ADR
0026](../adr/0026-federate-fabric-across-own-hosts-over-ssh.md) own the design.
This first slice provides configuration and diagnostics; remote lanes and
record federation follow in later slices.

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
     }
   }
   ```

   On `workshop`, use `local_host: "workshop"` and a `laptop` peer. Without a
   file, the local name is `local`. Missing `peers` or an empty object means
   local-only operation.
3. Configure the SSH aliases yourself. The destination may be an alias or
   `user@alias`; options, whitespace and shell metacharacters are refused.
   Host names use lowercase letters, digits and hyphens. Unknown JSON fields,
   duplicate keys and invalid versions are refused. Both timeouts must be
   positive and at most 86,400 seconds; connect timeout is an integer.
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
and sandbox probes; write-mode and capability-specific checks run at placement
in later slices. Socket checks measure an estimated v2 attempt temporary path
in bytes against the platform limit; a long project path can exceed it.
Registration reports the existing install check's Fabric-entry presence,
independently of whether the adapter executable is on PATH.

Offline or timed-out peers get typed rows and leave the overall command
successful. A deadline marks only that host unreachable for that call.
`AGENT_FABRIC_SSH_PROGRAM` overrides the SSH executable for test shims.
All supported MCP chairs can call `fabric_hosts` with `action: "list"` or
`"doctor"` and optional `hosts: ["workshop"]`.

## Machine contracts

Protocol version 1 accepts one UTF-8 JSON document on stdin, at most 65,536
bytes, followed by EOF. The request has `protocol_version`, `verb` and optional
`params`. Only `hello` and `doctor` are supported; doctor accepts optional
`project_path`. There is one newline-terminated JSON response on stdout:

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
`writes_allowed: false`. Future write callers must use the shared
`protocol_decision` function and the client's `write=True` guard.

Lists use `fabric.hosts.v1`; doctor uses `fabric.hosts.doctor.v1`. The shared
identifier helpers parse and format `id@host`; bare record IDs mean local.
A selector matching records on multiple hosts returns `ambiguous_selector`.
Host command selectors are host names, optionally written `host@host`.
