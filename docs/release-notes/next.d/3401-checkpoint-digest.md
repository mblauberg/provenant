## Fixes

### Pipelines and graphs

- Graph text is normalised exactly once, at ingress, by schema path, rejecting credentials in schema identities and object keys while preserving valid identities and normalising free-form fields, template documentation and extension values. Template substitutions and newly compiled expansions are scanned before admission to prevent credentials assembled from separate fragments. Stored bytes are verified directly on strict reads and digest checks without re-normalising, so saved graphs and deployed checkpoints verify on reload, resume and advance with matching plan digests. (#3401)
