# Spike: plugin manifests over the Provenant skills tree

Issue: [#773](https://github.com/mblauberg/provenant/issues/773). Parent: #743.
Status: complete, evidence only. Nothing in the live install path changed.
Date: 2026-09-02. Harnesses: Claude Code 2.1.258, codex-cli 0.146.0.

## Question

Both vendors now ship versioned plugins from a marketplace. What does a plugin
buy that `scripts/install-harness` does not, and what does it cost?

## What was built

All of it lives at the worktree root, so the repository itself is the plugin
root and `skills/`, `agents/` and `workflows/` are already in the conventional
places.

| File | Vendor | Purpose |
| --- | --- | --- |
| `.claude-plugin/plugin.json` | Claude Code | plugin manifest, names the four agents explicitly |
| `.mcp.json` | Claude Code | Fabric MCP server under `${CLAUDE_PLUGIN_ROOT}` |
| `packaging/marketplace/.claude-plugin/marketplace.json` | Claude Code | single-plugin private marketplace, `github` source |
| `.codex-plugin/plugin.json` | Codex | plugin manifest, skills plus MCP |
| `.codex-plugin/mcp.json` | Codex | Fabric MCP server, Codex path and env shape |
| `.agents/plugins/marketplace.json` | Codex | marketplace with a `local` source of `.` |

## What works

Claude Code:

- `claude plugin validate . --strict` passes. `claude plugin validate
  ./packaging/marketplace --strict` passes.
- `claude --plugin-dir .` loads the plugin. A probe session reported
  `provenant:`-prefixed variants of the skills and of the four agents alongside
  the symlink-installed copies, so skill and agent discovery both work with no
  entry in `~/.claude`.
- The manifest's MCP server registers under the scoped name
  `plugin:provenant:fabric`.
- `runtime/fabric/bin/fabric-mcp` completes an MCP `initialize` handshake when
  launched with `AGENT_FABRIC_PRODUCT_ROOT` pointed at the plugin root, so the
  shim at `~/.local/bin/provenant` is not needed by a plugin. The connection
  itself could not be confirmed inside the nested headless probe, where the
  already-installed user-level `fabric` server failed identically; the failure
  is therefore not attributable to the manifest.

Codex:

- `codex plugin marketplace add .` and `codex plugin add provenant@provenant`
  both succeed against a scratch `CODEX_HOME`, with nothing written to
  `~/.codex`. `codex plugin list --json` reports
  `provenant@provenant`, version `0.1.0`, installed and enabled.
- A `local` source of `.` is accepted, so the marketplace root and the plugin
  root may be the same directory. Claude Code has no equivalent: a marketplace
  entry source must start with `./`, so a repository cannot be both its own
  marketplace and its own plugin. The Claude marketplace here therefore points
  back at `github:mblauberg/provenant`.
- Codex plugin skill discovery could not be probed live: the account hit its
  usage limit before `codex exec` produced a turn.

## What a plugin buys

1. **No install step and no symlink stack.** Three receipt files
   (`.agent-harness-installation.json` and the agents and workflows variants),
   the per-entry symlinks under `~/.claude/skills` and `~/.codex/skills`, the
   per-file agent and workflow symlinks, the generated `CLAUDE.md` bootstrap,
   the two MCP config edits and the `~/.local/bin/provenant` shim copy all go
   away. `${CLAUDE_PLUGIN_ROOT}` replaces the shim's product-root resolution.
2. **Versioning and update.** `version` in the manifest gates updates, and both
   CLIs cache per version. The installer has no version concept at all; it
   tracks drift with sha256 digests instead.
3. **Namespacing.** Plugin skills and agents arrive as `provenant:<name>`, so
   they cannot collide with a user's own. The installer's answer to collision is
   a hard failure (`skill source name collision`).
4. **Distribution to other people.** A private marketplace, a GitHub source and
   an org-managed allowlist are all first-class. The installer assumes a local
   checkout the user already has.
5. **Managed force-enable.** `enabledPlugins` in Claude Code managed settings can
   force a plugin on, read-only to the user, and `strictKnownMarketplaces` can
   restrict which marketplaces are reachable at all. Notably `--plugin-dir`
   cannot override a managed force-enable or force-disable, so an org that
   force-enables Provenant also removes the local-override escape hatch that
   makes the current worktree workflow tolerable.

## What a plugin cannot do

1. **Custom-skills mixing.** `scripts/manage_installation.py` merges
   `~/.agents/custom-skills/<name>/` into the same target directory as the
   product skills, tracks the two sets separately in the receipt, and models the
   transitions between them (`custom-to-managed`, `custom-rebind`,
   `custom-retired` and the rest). A plugin has no equivalent. A user's own
   skills would have to become a second plugin or stay in `~/.claude/skills`,
   and the union view with ownership tracking is lost.
2. **HARNESS.md and the instruction bootstrap.** The installer writes
   `~/.claude/CLAUDE.md` and `~/.codex/AGENTS.md` naming `<instance>/AGENTS.md`
   and `<product>/HARNESS.md` by absolute path, and rebinds a stale
   `HARNESS.md` link when the product root moves. A plugin's `settings.json`
   accepts only `agent` and `subagentStatusLine`; there is no way to contribute
   global instructions. Every skill would have to carry its own pointer, or the
   constitution would have to be read through a skill, which changes the loading
   model.
3. **Instance seeds and model routing.** `config/model-routing.json` and
   `config/model-preferences.json` are copied once into `$instance_root/config/`
   and are thereafter user-owned; `scripts/model_route.py` reads them from
   `AGENT_FABRIC_INSTANCE_ROOT`. A plugin ships read-only content under a
   versioned cache path that changes on every update, so it can ship a default
   catalogue but cannot seed a file the user then edits. `userConfig` is the only
   plugin-side knob and it is a flat key/value dialog, not a JSON catalogue.
4. **The live tree property.** Codex copies the whole plugin into
   `<CODEX_HOME>/plugins/cache/<marketplace>/<plugin>/<version>/`. The measured
   copy was **146 MB**, because the repository root carries `node_modules`,
   `tests`, `evals` and `runtime`. Nothing in the checkout takes effect until the
   version is bumped and the plugin reinstalled. Today an edit under
   `skills/` is live in the next session through the symlink. Losing that would
   change how the harness is developed.
5. **Codex has no agents or workflows surface.** No Codex plugin in the local
   cache declares an `agents` key; the installer already guards both behind
   `if [[ "$PLATFORM" == "claude" ]]`. A plugin does not close that gap.
6. **One MCP file cannot serve both.** Claude wants
   `command: "${CLAUDE_PLUGIN_ROOT}/..."` with an `env` map; Codex wants a
   relative `command` with `cwd` and an `env_vars` allowlist. Two files are
   required, and the Fabric server needs `node_modules/tsx` present in the
   plugin root, so a source-distributed plugin needs a build or vendoring step
   that the symlink installer never needed.

## Other findings

- `scripts/public_release_check.py` lists `plugins/marketplace.json` in
  `FORBIDDEN_TRACKED`. The Codex marketplace path used here,
  `.agents/plugins/marketplace.json`, does not match that entry, but any move to
  a plugin layout should revisit the ban deliberately rather than by accident.
- `skills/_shared` has no `SKILL.md` and is skipped by plugin skill discovery.
  The installer treats it as a managed catalogue entry and links it explicitly,
  so a plugin needs the shared Python package to be reachable another way.

## Recommendation

**No-go for now, revisit when the two gaps close.** The split installer stays the
live path. A plugin removes real machinery, but it cannot carry the
`custom-skills` merge or the `HARNESS.md` and instance-seed bootstrap, and the
Codex copy-install would cost the live-tree property that the harness is
developed against. Adopt plugins when either (a) Provenant is distributed to
people who do not have a checkout, at which point the marketplace and managed
force-enable become the point, or (b) plugin manifests gain a way to contribute
global instructions and a user-editable config seed. Keep these manifests on the
spike branch as the starting point; they validate and install today.
