## Fixes

### Pipelines and graphs

- Refused launches, retries and graph edits preserve the checkpoint and requester PATH. Graph admission checks the full prospective loop expansion before saving. Credential refusals retain safe schema positions through the lane relay without echoing caller keys or extension values. (#3391)
- Resuming graphs preserves cancellation and checkpoint corruption diagnostics, retries continuation pauses after budget increases, and retains valid extension lists within the checkpoint envelope. Malformed planner fragments keep their non-structural refusal cause. (#3391)
