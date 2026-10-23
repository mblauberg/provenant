# Landing lease from a repository script

Status: current
Applies to: `provenant lease`, `fabric landing-*`, repository landing tools

A repository's own landing script takes Fabric's landing lease with
`provenant lease`. It does not start `fabric-mcp` or speak MCP over stdio:
the server path and protocol version are internal and may change. The verbs,
flags, JSON fields and exit codes below are the contract. The verbs share one
store with the `fabric_landing_lease` MCP tool, so a chair's MCP lease and a
script's lease serialise against each other. The [Fabric
Rust design](../specs/rust-port/design/fabric.md) owns the
semantics.

## Call shapes

Run each command from the landing checkout. A linked worktree and its primary
checkout resolve to the same project, so they share the same lease.

```sh
provenant lease acquire land --branch main --remote origin --label lander --json
# {"acquired":true,"holder":"lander/land","generation":7,"expected_sha":"<40 hex>",
#  "expires_at":"<ISO 8601 UTC>","label":"lander","session":"land"}
provenant lease verify land 7 --branch main --label lander --json
provenant lease push land 7 main --remote origin --label lander
provenant lease release land 7 --label lander --json
provenant lease status --json
```

`acquire` takes exactly one of `--expected-sha` or `--branch`. With
`--branch` it reads the remote tip through `git ls-remote`. `--seconds` sets
the lease length, from 1 to 3,600 seconds (default 300). A successful `push`
releases the lease itself, so call `release` only after an aborted landing.

## Identity

The holder is `<label>/<session>`. The label is `--label`, otherwise
`AGENT_FABRIC_LABEL`, otherwise the provider seat. Acquire echoes the `label`
it used. Pass that label to every later call, because a child process with a
different environment would otherwise resolve to a different holder and see a
stale lease.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | ok, including `release` that returns `released: false` with reason `expired` or `not_held` |
| 1 | refused or stale: `verify` reports `reason` `stale`, `sha_mismatch` or `sha_moved`; also any store or Git error |
| 2 | usage error |
| 3 | `acquire` while another holder's lease is live (`holder`, `until`), or `release` during a push (`reason: pushing`) |

With `--json`, each verb prints one object on stdout for codes 0, 1 and 3. A usage
error or an error before any outcome, such as a store that cannot be opened,
prints only to stderr. Check the exit code first. An empty stdout never means
"no lease".

## Limits

The lease serialises landings on one host. It is not a cross-host mutex.
`push` checks the remote SHA and pushes with `--force-with-lease`, which
guards against a push from another host or a merge made in the GitHub UI.
