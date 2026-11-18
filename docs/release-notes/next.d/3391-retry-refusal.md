## Fixes

### Pipelines and graphs

- Refused launches, retries and graph edits preserve the checkpoint and requester PATH. Graph admission checks the full prospective loop expansion before saving. Policy refusals retain safe schema positions through the lane relay without echoing caller keys or extension values. (#3391)
- Resuming graphs preserves cancellation and checkpoint corruption diagnostics, retries continuation pauses after budget increases, and retains valid extension lists within the checkpoint envelope. Malformed planner fragments keep their non-structural refusal cause. (#3391)
- Cloud placement refusals retain the cloud launch remedy. Secret-shaped retry, patch, launch, template and child inputs warn and proceed by default; warnings name schema fields and never values. Shape-only matches never refuse, even under a strict policy. Exact declared credentials refuse atomically with a typed cause. Authored inputs preserve unregistered examples and mask registered credential values. Captured output masks exact registered values while preserving unregistered shapes and assignments; identities and keys refuse only for registered credentials. (#3391)
