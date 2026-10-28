## Behaviour changes

- Explicit checkpoint saves register exact chair identities for session discovery, with checkpoint consistency reported after an interrupted save. Session reads allocate no state.

- Checkpoint saves refuse unreadable discovery records and changes to a registered chair's host or actor. MCP chairs without an explicit agent label can use `workflow session save --client HARNESS` to match their MCP inbox identity.
