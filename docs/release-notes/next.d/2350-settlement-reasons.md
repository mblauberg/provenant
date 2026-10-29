## Fixes
### Lanes and settlement

- Cancelled lanes report their cancellation reason, identity failures include requested and observed models, and failed lanes retain declared-stop signals (#2350).
- Model lanes that finish without a final handoff record `handoff_missing` without changing their outcome; command lanes and failed lanes do not. Contradictions between a done handoff and cancellation or required review are noted (#2350).
- Claude identity is taken from the assistant message matching the final answer, so an earlier helper model does not mask the answering model (#2350).
- OpenCode HTTP 500 responses that explicitly report overload are classified as provider capacity; other 500 errors remain generic failures (#2350).
