# Fabric MCP registration

Status: legacy registry editor for existing client registrations
Applies to: `scripts/configure-fabric-mcp.py`

The `provenant` CLI owns release and managed-file operations. `provenant install`
manages harness-specific MCP, hook and link entries; this page documents the
separate legacy registry editor for existing registrations.

`scripts/configure-fabric-mcp.py` remains a separate legacy registry editor. It
can add, check or repair registry entries when the stable shim is already
installed. Its `--agents-home` argument selects instruction source paths; it
does not add a checkout path to registrations or follow a product-root pointer.
Pass `--instance-root` when the registration needs an explicit instance root.

## What gets registered
## What gets registered

The legacy registration format names the stable shim at
`${PROVENANT_BIN_DIR:-$HOME/.local/bin}/provenant` and supplies the client seat.
It stores no product checkout path. The old pointer-based relocation procedure
is retired; Rust-managed registrations are outside this editor's scope.

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

The standalone editor leaves `AGENT_FABRIC_PRODUCT_ROOT` out of registrations.
Its optional `--instance-root` argument supplies `AGENT_FABRIC_INSTANCE_ROOT`;
without it, that variable is absent.

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

When `CLAUDE_CONFIG_DIR` targets an alternate account directory, the Rust install
command links its `settings.json` to `~/.claude/settings.json`. The shared
`settings.json` link means settings changed in one account apply to all linked
accounts, and if Claude replaces the link with a regular file the accounts stop
sharing; rerunning `provenant install` does not overwrite it.

Each client has its own seat (`claude`, `codex`, `cursor`, `agy`, `kiro`, or
`opencode`) and therefore its own default inbox. A seat does not establish the
model family chosen by a broker.

Agy holds its own `agy` seat so its client records remain addressable. The seat
is routing metadata, not proof that a Gemini or Google model ran. For a
cross-family review, count the model family only from the exact provider/model
fields in the direct-dispatch receipt; a non-Google model selected through Agy
is not a qualifying Gemini leg.

OpenCode's `instructions[]` can include the instance `AGENTS.md` and the
product `HARNESS.md`. The standalone editor preserves unrelated entries and
requires a string array; pass `--agents-home` and `--instance-root` to select
those paths explicitly. It does not rebind paths from a product-root pointer.
OpenCode JSONC with comments is read safely. If adding Fabric would require a
rewrite that discards those comments, the editor leaves the file intact and
prints the exact `instructions` and `mcp.fabric` entries to add manually.
With `--platform all`, a detected client's configuration conflict produces a
per-client skip warning; explicitly named clients still fail.

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
