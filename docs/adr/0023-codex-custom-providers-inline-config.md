# ADR 0023 — Codex custom providers arrive inline, not by relaxing `--ignore-user-config`

**Status:** Accepted 2026-09-02 (issue [#772](https://github.com/mblauberg/provenant/issues/772))

**Extends:** [ADR 0021](0021-configured-workspace-dispatch-boundaries.md), which
makes `skills/orchestrate/scripts/cf_dispatch.sh` the sole owner of provider
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
`codex`, `cf_dispatch.sh` adds:

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

## Non-goals

This decision does not add an endpoint profile for any particular provider, does
not read or forward credentials, and does not give any other adapter a new
config surface.
