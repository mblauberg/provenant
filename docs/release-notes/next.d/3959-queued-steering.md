## Fixes
### Fabric
- Steering a running lane queues the note for its next turn without interrupting the attempt. When a retained session is unavailable, the configured fallback determines whether to start fresh with a handoff or leave the note without a successor. (#3959)
