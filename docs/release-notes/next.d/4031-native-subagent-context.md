## Fixes

### Native sessions and hooks

- A native subagent no longer receives the chair's checkpoint, standing grants or account-switch handoff on `SessionStart`. A non-empty `agent_id` or agent transcript path makes the chair hooks abstain before consuming a handoff or recording chair attribution, and makes the advisory checkpoint hooks emit no context. `agent_type` alone does not mark a subagent because it is also present for top-level `--agent` sessions. Lanes were already excluded by their lane identity. (#4031)
