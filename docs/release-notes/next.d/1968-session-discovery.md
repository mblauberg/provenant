## Behaviour changes

- Saving a checkpoint registers the chair's exact identity for session discovery, and a checkpoint left inconsistent by an interrupted save is reported. Reading sessions changes no state. A failure to publish discovery after the save warns and does not fail the save. The next save repairs it (#1968).
- A chair's identity is now the same as its Fabric MCP inbox actor across session saves, hooks and handoff. An explicit label wins over the harness client, so unlabelled Claude and Codex chairs on one host have different actors. `save --rebind` keeps the last 8 binding changes. Lanes cannot register chairs (#1968).
- General CLI commands keep their configured actor and `identity.client` (default `agent`), whatever the harness. To recover an old chair binding, save with `--client HARNESS --rebind`. Handoff recovery commands ask for an explicit harness when none is given (#1968).
