## Behaviour changes

- Explicit checkpoint saves register exact chair identities for session discovery, with checkpoint consistency reported after an interrupted save. Session reads allocate no state.

- Chair identity equals the Fabric inbox actor across CLI, MCP callers, hooks and handoff, with explicit labels taking precedence over the harness client. Unlabelled Claude and Codex chairs on one host have different actors. `save --rebind` keeps the last 8 binding changes. Lanes cannot register chairs. Discovery publication is advisory: a failure after the manifest commit warns without failing the save, and the next save repairs it.
