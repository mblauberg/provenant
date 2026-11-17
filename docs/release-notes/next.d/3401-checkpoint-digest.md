## Fixes

### Pipelines and graphs

- Graph text is normalised exactly once, at ingress, by schema path, preserving schema identities while normalising free-form fields, template documentation and extension values. Stored bytes are verified directly on strict reads and digest checks without re-normalising, so saved graphs and deployed checkpoints verify on reload, resume and advance with matching plan digests. (#3401)
