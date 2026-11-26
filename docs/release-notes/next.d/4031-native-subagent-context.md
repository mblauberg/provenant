## Fixes

### Native sessions and hooks

- A native subagent no longer receives the chair's checkpoint, standing grants or account-switch handoff on `SessionStart`. A hook payload that marks an agent (a non-empty `agent_type` or agent transcript path) now makes the chair hooks abstain before consuming a handoff or recording chair attribution, and makes the advisory checkpoint hooks emit no context. Lanes were already excluded by their lane identity. (#4031)
