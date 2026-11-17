## Fixes

### Pipelines and graphs

- Refused launches, retries and graph edits preserve the checkpoint and requester PATH. Graph admission checks the full prospective loop expansion before saving. Credential refusals retain safe schema positions through the lane relay without echoing caller keys or extension values. (#3391)
