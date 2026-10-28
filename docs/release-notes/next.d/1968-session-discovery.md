## Behaviour changes

- Explicit checkpoint saves register exact chair identities for session discovery, with checkpoint consistency reported after an interrupted save. Session reads allocate no state.

- Chair identity uses one configured Fabric label across CLI, MCP callers, hooks and handoff. `save --rebind` records deliberate binding changes. Lanes cannot register chairs. Discovery publication is advisory: a failure after the manifest commit warns without failing the save, and the next save repairs it.
