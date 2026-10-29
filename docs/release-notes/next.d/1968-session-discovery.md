## Behaviour changes
### Chair, inbox and console

- Saving a checkpoint registers the chair for session discovery and reports a checkpoint left inconsistent by an interrupted save. A failure to publish discovery warns without failing the save, and the next save repairs it (#1968).
- A chair's identity is now its Fabric MCP inbox actor across session saves, hooks and handoff, and an explicit label wins over the harness client. Unlabelled Claude and Codex chairs on one host therefore have different actors. Other CLI commands keep their configured actor and `identity.client` (default `agent`). To recover an old binding, save with `--client HARNESS --rebind` (#1968).
