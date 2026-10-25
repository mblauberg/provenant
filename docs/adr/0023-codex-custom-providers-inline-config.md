# ADR 0023 — Codex custom providers arrive inline, not by relaxing `--ignore-user-config`

> Legacy source references below describe the pre-rewrite implementation.
> Use `provenant help` for current Rust commands and the accepted
> [Rust designs](../specs/README.md) for owners.

**Status:** Accepted 2026-09-02 (issue [#772](https://github.com/mblauberg/provenant/issues/772))

**Extends:** [ADR 0021](0021-configured-workspace-dispatch-boundaries.md), which
makes `skills/orchestrate/scripts/cf_dispatch.py` the sole owner of provider
invocation.

## Context

Every Codex dispatch runs `codex exec ... --ignore-user-config`. That flag
exists so a dispatched run cannot inherit the operator's own
`$CODEX_HOME/config.toml`: an unrelated model, sandbox setting, hook, MCP server
or service tier in a personal config would silently reshape a recorded run, and
the receipt would still claim the route the catalogue resolved.

A custom `model_provider` is defined in exactly that file. So the flag that
keeps a run honest is also the flag that put an OpenAI-compatible open-model
endpoint out of reach. Issue #745 and [ADR 0021](0021-configured-workspace-dispatch-boundaries.md)
gave the catalogue an endpoint profile (base URL plus the *name* of the
environment variable holding the token), and the `claude` adapter already uses
one. Codex needed the same reach.

Two options were available: relax `--ignore-user-config` on endpoint routes, or
pass the provider inline as `-c` overrides.

## Decision

Pass the provider inline. `--ignore-user-config` stays on every Codex route,
ordinary and endpoint alike.

When a dispatch names an endpoint profile whose `adapters` list includes
`codex`, `cf_dispatch.py` adds:

```
-c model_providers.provenant_endpoint.name=<profile name>
-c model_providers.provenant_endpoint.base_url=<profile base_url>
-c model_providers.provenant_endpoint.env_key=<profile token_env>
-c model_providers.provenant_endpoint.wire_api=<profile wire_api>
-c model_provider=provenant_endpoint
```

The provider identifier is the fixed literal `provenant_endpoint`. The profile
name is carried as the provider's display name instead, so a profile name
containing a character a TOML dotted path would have to quote cannot reshape the
override. The endpoint profile gains an optional `wire_api` field, validated in
`scripts/model_route.py` against the wire formats a provider CLI can be told to
speak; anything else is a configuration error and the route is rejected before
any process starts.

The token is named, never carried. `env_key` tells Codex which environment
variable to read, so the credential never appears in an argument vector, a route
record, a receipt or a run file.

### Reasoning

- Relaxing the flag buys one provider setting at the price of every other
  setting in the operator's config. The blast radius is unbounded and invisible:
  nothing in the receipt would show that a personal `model_reasoning_effort`,
  sandbox override or MCP server had joined the run.
- The inline form is bounded and legible. Exactly the fields the catalogue
  declares reach the CLI, and they are visible in the argument vector a test can
  assert on.
- It keeps the catalogue as the single source of route truth, matching how the
  `claude` adapter consumes the same profile.
- The failure mode is honest: a profile that is malformed or names an unusable
  wire format fails route resolution rather than falling back to whatever the
  user's config happens to say.

## Verification

Checked against `codex-cli 0.146.0` on the development machine, since the
interaction between `-c` and `--ignore-user-config` is not documented:

- `codex exec --ignore-user-config ... -c model_provider=probe-missing` fails
  with `Error: Model provider 'probe-missing' not found`, so `-c model_provider`
  is read with the flag set.
- Defining the provider inline as well produces a session reporting
  `provider: probe` and a request to the configured base URL, so the whole
  inline provider is honoured with the flag set.

That version also rejects `wire_api = "chat"` at config load
(`'wire_api = "chat"' is no longer supported`), so a Codex endpoint profile must
declare `"wire_api": "responses"` and an endpoint that speaks only Chat
Completions is out of reach through this adapter until Codex restores that wire
format. This is a provider constraint, not a harness one; the field is validated
rather than hard-coded so the catalogue can follow the CLI.

## Consequences

- An operator adds a Codex-reachable endpoint by adding one profile to
  `config/model-routing.json` with `"adapters": ["codex"]` and a `wire_api`, and
  by exporting the named token variable. No code change and no personal Codex
  config are involved.
- `config/adapter-compatibility.yaml` admits the endpoint families for
  `codex-app-server`, as it already does for `claude-agent-sdk`. Without
  `--endpoint`, the catalogue's `fixed_model_family` still pins Codex to
  `openai`.
- An endpoint route carries no reasoning effort, matching the existing endpoint
  rule: a third-party endpoint exposes no effort control the harness can claim.
- Ordinary Codex dispatch is unchanged and still inherits nothing.

## Amendment 2026-10-16: ordinary lanes inherit the user config

Context and Decision above describe the clean configuration, which is now the
assurance behaviour only. A lane reached through `dispatch_run` with
`--intent ordinary` (the Fabric default) previously lost the operator's MCP
servers, plugins, hooks, execpolicy rules, memories and subagent settings, and
so had less reach than a native Codex subagent. It now loads them, and the
applied block of the receipt records `config: inherited` or `config: clean`.

- `assurance` intent (the `cf_dispatch.py` default), capability lanes (which run
  under their own `CODEX_HOME`) and `CF_DISPATCH_CODEX_CONFIG=clean` keep
  `--ignore-user-config --ignore-rules` and `allow_login_shell=false`.
  `CF_DISPATCH_CODEX_CONFIG` accepts `clean` or `inherited`; any other value is
  an error.
- Every route and permission override Provenant passes with `-c` (permission
  profile, model, effort, approval policy, service tier, endpoint provider)
  still wins over the user file, so a user config that grants unrestricted
  filesystem access does not widen a lane: read-only lanes stay unable to write
  the workspace and writers stay inside their worktree.
- An inheriting lane also sets `web_search="live"` and `notify=[]`, and sets
  `default_tools_approval_mode="approve"` for each server it keeps, since no one
  is present to approve a tool call. Per-server settings go in one
  `mcp_servers={...}` parent-table override: Codex splits a dotted `-c` key on
  every dot even when quoted, so `mcp_servers."a.b".enabled=false` fails.
- Read-only and every other non-writer lane runs no hook and no stdio MCP
  server. Hooks (user, trusted project and plugin) and stdio servers run outside
  Codex's command sandbox and the permission profile, so the lane sets
  `features.hooks=false` (receipt: `applied.hooks_disabled`) and
  `enabled=false` for each server that executes a local process (receipt:
  `applied.mcp_disabled`): stdio servers, and remote servers with a command
  field such as `http_headers_helper`. It keeps plain remote (`url`) servers. A
  plugin-supplied server has no config-table transport, so a per-server override
  makes Codex exit `invalid transport`; when one executes, the lane sets
  `features.plugins=false` instead (receipt: `applied.plugins_disabled`). A
  server name that cannot be spelt in an override sends the lane clean. The
  server inventory comes from `codex mcp list --json` run in the
  lane's directory, which resolves every layer Codex loads (user config, trusted
  project `.codex/config.toml` files, plugin servers); a second listing under
  `features.plugins=false` identifies the plugin servers. Discovery has an 8
  second budget. The decision is taken once per plan and reused when the argv is
  rebuilt (context ceiling, effort retry, resume), so receipts match the argv. If
  discovery fails or times out the
  lane runs clean (`--ignore-user-config --ignore-rules`) and the receipt carries
  `applied.config_note`, rather than inheriting blind.
- Accepted boundary for worktree writers: a writer keeps every hook and server, as
  a native Codex session does, and these run with the operator's privileges
  outside the worktree confinement; a stdio server or hook can write anywhere the
  operator can. This is a deliberate parity choice, not a guard. The receipt
  records it as `applied.hooks_unconfined` and `applied.mcp_unconfined` (the
  servers that execute; `applied.mcp_unapproved` lists plugin servers, which
  take no override). Assurance and capability lanes, which run clean, are
  unaffected.
- Every native-route lane pins `model_provider="openai"` and clears
  `openai_base_url`, so a provider or base URL in the user file cannot redirect
  the route and misattribute the receipt. An endpoint route keeps its own pin.
- A lane may write Provenant's own state directory (`$XDG_STATE_HOME/provenant`,
  holding the admission lock), created at execution time and recorded as
  `applied.write_boundary.state_dir`. The grant is skipped, with
  `state_dir_skipped` noting why, when the path is a symlink, holds a glob
  character, or equals, contains or lies inside the cwd, workspace, worktree or
  an add-dir.

## Non-goals

This decision does not add an endpoint profile for any particular provider, does
not read or forward credentials, and does not give any other adapter a new
config surface.
