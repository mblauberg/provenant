# Fabric MCP registration

Status: current
Applies to: `scripts/configure-fabric-mcp.py` and `runtime/fabric/bin/fabric-mcp`

`install-harness --platform all` registers Fabric MCP for Claude Code and Codex,
plus OpenCode, Agy, Cursor and Kiro when their home directories are present.
It also projects skills into each present client home, and installs an explicit
bootstrap and HARNESS link for OpenCode and Agy. Add `--mcp-clients all` to
configure all six registries even when the optional clients are absent.
The shipped `agents/*.md` use Claude Code's agent format and are installed only
there.
`scripts/configure-fabric-mcp.py --platform all` also configures
all six directly; it does the registry writing and can be run on its own to
add a client, check the registrations or repair one.

## What gets registered

The registered command is the managed stable shim at
`${PROVENANT_BIN_DIR:-$HOME/.local/bin}/provenant`. With no arguments and
`AGENT_FABRIC_SEAT` set, that shim resolves the product root and execs
`runtime/fabric/bin/fabric-mcp`. Registering the shim rather than the checkout
path means moving the checkout does not invalidate six client registries: re-run
`scripts/install-harness` from the new checkout and the pointer at
`<instance-root>/.agent-fabric/product-root.json` is updated atomically.

Every entry carries these three environment variables by default:

| Variable | Purpose |
| --- | --- |
| `AGENT_FABRIC_STATE_DIRECTORY` | Where `fabric.sqlite3` lives |
| `AGENT_FABRIC_SEAT` | Client routing metadata recorded with the Fabric identity; not model-family proof |
| `AGENT_FABRIC_CLIENT_LABEL` | The label the provider is addressed by |

No entry binds a project. The client's working directory selects a repository;
its ordinary registered worktrees share one Fabric project. This lets one
registration cover every directory you work in. Clients must preserve the
workspace working directory; Fabric has no manual project override.

The registered client label is a shared default address. Give concurrently
working agents distinct `AGENT_FABRIC_LABEL` values when they need separate
inboxes; agents that intentionally share a label compete for the same claims.

`AGENT_FABRIC_PRODUCT_ROOT` is a CLI-only control and is absent from global
client registrations. When a non-default instance root is selected,
`install-harness` also records that explicit `AGENT_FABRIC_INSTANCE_ROOT` in
the registration so the stable shim resolves the same instance; default-root
registrations need no extra variable.

## The six clients

Registration exposes coordination and, on current clients, the thin dispatch
and batch façade. It does not activate a provider: Kiro remains
dormant behind its compatibility gate until separately enabled. OpenCode is an
ordinary implemented dispatch adapter when the CLI is installed; pass an
explicit `opencode/<model>` slug. Changing an MCP registry never overrides
adapter compatibility.

| Client | Global registry |
| --- | --- |
| Agy | `~/.gemini/config/mcp_config.json` |
| Claude Code | `~/.claude.json` |
| Codex | `~/.codex/config.toml` |
| Cursor | `~/.cursor/mcp.json` |
| Kiro | `~/.kiro/settings/mcp.json` |
| OpenCode | `~/.config/opencode/opencode.jsonc` |

Each client has its own seat (`claude`, `codex`, `cursor`, `agy`, `kiro`, or
`opencode`) and therefore its own default inbox. A seat does not establish the
model family chosen by a broker.

Agy holds its own `agy` seat so its client records remain addressable. The seat
is routing metadata, not proof that a Gemini or Google model ran. For a
cross-family review, count the model family only from the exact provider/model
fields in the direct-dispatch receipt; a non-Google model selected through Agy
is not a qualifying Gemini leg.

OpenCode's `instructions[]` includes the instance `AGENTS.md` and the product
`HARNESS.md`. The installer preserves unrelated entries, rejects a conflicting
doctrine path, and rebinds a prior product path recorded by the instance's
product-root pointer when the checkout moves. Standalone MCP configuration
uses the pointer for the current product path when `--agents-home` is omitted.
Re-run `install-harness` after
relocation so the literal OpenCode paths and pointer agree.

`config/model-routing.json` remains instance-owned. Install and validation name
product catalogue differences by key; run `install-harness --platform all
--refresh-routing` to back up the instance file to `model-routing.json.bak-<date>`
and apply product sections while retaining instance-only adapters, endpoints,
and extra alias models.

    scripts/configure-fabric-mcp.py --platform all
    scripts/configure-fabric-mcp.py --platform codex
    scripts/configure-fabric-mcp.py --check

## Verify

Each client reports the server as `fabric`. A new session may be needed after a
registry changes.

```sh
claude mcp list
codex mcp list
cursor-agent mcp list
kiro-cli mcp list
agy mcp list
opencode mcp list
```

The Agy CLI renders `mcp list` in a TUI and fails without a TTY. Headless,
inspect the `fabric` object in `~/.gemini/config/mcp_config.json` and confirm
its command instead.

## When a write fails partway

Registering across several clients is a first-client atomic install followed by
the rest. If the first client cannot be written, nothing is committed and the
script fails without touching a registry. If a later client fails after an
earlier one committed, the script reports `partial-state` on stderr with the
committed clients, the remaining clients, the configuration at fault and the
recovery step, then exits with exit code `4`.

Recovery is always the same shape: reconcile the reported configuration and any
recovery file left beside it, then re-run with `--platform all`. The script is
idempotent, so re-running over already-correct entries changes nothing.
