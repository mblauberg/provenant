## Behaviour changes

- Explicit checkpoint saves register exact chair identities for session discovery, with checkpoint consistency reported after an interrupted save. Session reads allocate no state.

- Chair identity equals the Fabric MCP inbox actor across session saves, hooks and handoff, with explicit labels taking precedence over the harness client. Unlabelled Claude and Codex chairs on one host have different actors. `save --rebind` keeps the last 8 binding changes; longer histories are truncated on read. Lanes cannot register chairs. Discovery publication is advisory: a failure after the manifest commit warns without failing the save, and the next save repairs it.

- General CLI commands retain their configured actor and `identity.client` (default `agent`) regardless of harness markers. To recover an old chair binding, save with `--client HARNESS --rebind`. Handoff recovery commands request an explicit harness when none was supplied.
