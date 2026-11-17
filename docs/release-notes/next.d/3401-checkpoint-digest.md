## Fixes

### Pipelines and graphs

- Graph text is normalised exactly once, at ingress, by field type, preserving schema identities while normalising parameter and prompt content. Stored bytes are verified directly on strict reads and digest checks without re-normalising, so saved graphs and deployed checkpoints verify on reload, resume and advance with matching plan digests. (#3401)
