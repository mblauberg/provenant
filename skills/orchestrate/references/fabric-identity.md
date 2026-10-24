# Fabric identity

Run `provenant fabric identity --json` in the intended workspace, or select it
with global `--project PATH`. Verify `data.project`, `data.cwd`, `data.common_dir`
and `data.inbox` before claiming work. Linked Git worktrees share a random
host-local project ID; their working directories remain distinct. Confinement
verifies Git discovery, and config owns the canonical project binding.

`PROVENANT_AGENT_LABEL` selects the inbox, then Fabric's `agent_label`, then
`client_label`. Use distinct labels for independent consumers. A shared label
shares one read cursor. Labels never establish a provider or model route.

Cross-host project keys derive from the canonical home-relative location.
Outside home, identity remains local. A configured remote project home refuses
local coordination until home routing is available.
