# Fabric identity

Call `fabric_identity` in the intended workspace. Verify `project`, `cwd`,
`common_dir` and `inbox` before claiming work. Linked Git worktrees share a random
host-local project ID; their working directories remain distinct. Confinement
verifies Git discovery, and config owns the canonical project binding.

`server_version` is the version of the MCP server that answered. A reinstall
does not restart a running server, so compare it with `provenant --version`. If
they differ, restart the Fabric MCP server, for example by restarting the agent
session, before relying on its output.

`PROVENANT_AGENT_LABEL` selects the inbox, then Fabric's `agent_label`, then
`client_label`. Use distinct labels for independent consumers. A shared label
shares one read cursor. Labels never establish a provider or model route.

Cross-host project keys derive from the canonical home-relative location.
Outside home, identity remains local. A configured remote project home refuses
local coordination until home routing is available.
