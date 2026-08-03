# Fabric MCP registration

Status: current
Applies to: `scripts/configure-fabric-mcp.py` and `runtime/fabric/bin/fabric-mcp`

`install-harness` registers the Fabric MCP server for every client it finds.
`scripts/configure-fabric-mcp.py` does the registry writing and can be run on
its own to add a client, check the registrations or repair one.

## What gets registered

The registered command is the managed stable shim at
`${PROVENANT_BIN_DIR:-$HOME/.local/bin}/provenant`. With no arguments and
`AGENT_FABRIC_SEAT` set, that shim resolves the product root and execs
`runtime/fabric/bin/fabric-mcp`. Registering the shim rather than the checkout
path means moving the checkout does not invalidate six client registries: re-run
`scripts/install-harness` from the new checkout and the pointer at
`<instance-root>/.agent-fabric/product-root.json` is updated atomically.

Every entry carries exactly three environment variables:

| Variable | Purpose |
| --- | --- |
| `AGENT_FABRIC_STATE_DIRECTORY` | Where `fabric.sqlite3` lives |
| `AGENT_FABRIC_SEAT` | Which provider this client is |
| `AGENT_FABRIC_CLIENT_LABEL` | The label the provider is addressed by |

No entry binds a project. The client's working directory decides which project
a call belongs to, which is what lets one registration cover every directory you
work in. `AGENT_FABRIC_PROJECT_PATH` is not a fourth global variable: it belongs
only in an explicit, separately managed project-scoped entry for a client that
cannot preserve the workspace working directory, and must never be reused
globally.

`AGENT_FABRIC_PRODUCT_ROOT` and `AGENT_FABRIC_INSTANCE_ROOT` are CLI-only
controls and should be absent from every global client registration.

## The six clients

| Client | Global registry |
| --- | --- |
| Agy | `~/.gemini/config/mcp_config.json` |
| Claude Code | `~/.claude.json` |
| Codex | `~/.codex/config.toml` |
| Cursor | `~/.cursor/mcp.json` |
| Kiro | `~/.kiro/settings/mcp.json` |
| OpenCode | `~/.config/opencode/opencode.jsonc` |

Claude Code and Codex take the `claude` and `codex` seats. Cursor, Agy and Kiro
share the `codex` seat by design while keeping their own client label, so their
messages are addressed separately without inventing a provider identity.

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
