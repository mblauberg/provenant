## Fixes

### CLI and install

- `output GRAPH` now selects a unique top-level composite result, exposes its declared typed outputs, and lists only valid top-level choices when the graph has several. Composite output has no synthetic attempt identity. (#3322)

### Pipelines and graphs

- Graph status reports review verdict, finding count and finding availability separately from execution settlement. (#3322)
