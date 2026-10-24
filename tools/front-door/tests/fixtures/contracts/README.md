# Harness MCP contract observations

These fixtures come from isolated command-line observations on macOS arm64. Every client process received `env -i` with fresh `/private/tmp` values for `HOME`, `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, and the XDG config/data/state/cache directories. The working directory was also fresh under `/private/tmp`. No real harness home/config was read, no provider was called, and no user credentials were present in the child environment.

`/private/tmp/provenant-contracts.<random>` was normalised to `/FIXTURE_ROOT` in the fixture data. The fake executable only logged `argv` and returned MCP initialize, tools/list, resources/list, and prompts/list responses; it made no network calls and exposed no tools.

| Harness | Version | Capture and parsed expectation |
| --- | --- | --- |
| Claude Code | 2.1.310 | `claude mcp list` read the isolated `CLAUDE_CONFIG_DIR/.claude.json`, health-checked the fake stdio server, reported Connected, and launched it with the exact recorded argv. |
| Codex CLI | 0.163.0 | `codex mcp list --json` parsed the isolated `CODEX_HOME/config.toml` `[mcp_servers.fabric]` entry as stdio command plus ordered args. Listing did not launch the child, so process argv is not claimed as captured. |
| agy | 1.4.6 | `agy mcp list` parsed `$HOME/.gemini/config/mcp_config.json` `mcpServers.fabric` as stdio command plus ordered args. Listing did not launch the child, so process argv is not claimed as captured. |
| Cursor | unavailable | `command -v cursor` found no executable. No real capture. Cursor's official docs show `~/.cursor/mcp.json` with `mcpServers`, `command`, `args`, and `env`: https://prod.cursor.com/help/customization/mcp. This is documentation-derived only. |
| OpenCode | v2.0.32 | `command -v opencode` found the executable, but a bounded isolated `opencode mcp list` did not complete: its managed service reported port 49374 already occupied. A bounded foreground-server retry did not yield a listing or launch the probe. No config/stdio behavior is claimed as captured. Official v2 docs describe a local `command` array and `environment`, with `disabled` rather than `enabled`: https://dev.opencode.ai/v2/docs/mcp-servers/. Documentation-derived; verify against a fixture before relying on the design table's `enabled = true`. |

## Files

- `claude-code-2.1.310/`: observed config shape, sanitised listing, and directly captured stdio argv.
- `codex-cli-0.163.0/`: observed TOML config shape and sanitised `mcp list --json` output.
- `agy-1.4.6/`: observed JSON config shape and sanitised `mcp list` output.

The config fixtures are sanitised test vectors: substitute `/FIXTURE_ROOT` with a temp directory before executing them. Do not treat the Codex or agy config's `args` array as a process capture; their listing commands only parsed and displayed it.
