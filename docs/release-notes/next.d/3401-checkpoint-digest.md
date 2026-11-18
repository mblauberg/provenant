## Fixes

### Pipelines and graphs

- Graph text is normalised exactly once, at ingress, by schema path, rejecting credentials in schema identities and object keys while preserving valid identities and normalising free-form fields, template documentation and extension values. Template substitutions and newly compiled expansions are scanned before admission to prevent credentials assembled from separate fragments. Stored bytes are verified directly on strict reads and digest checks without re-normalising, so saved graphs and deployed checkpoints verify on reload, resume and advance with matching plan digests. (#3401)

- Checkpoint writers, readers and fixtures share the runtime digest functions. Hand-built front-door and CLI fixtures now carry validated plan identities, including synthetic terminal graphs used to check pending-index recovery. Strict digest and topology verification remain unchanged. (#3400, #3401)
- Resume by a source path or document hash selects the journal’s sealed execution, as resume by graph ID does, preserving admitted template metadata and pinned bases. (#3400)
