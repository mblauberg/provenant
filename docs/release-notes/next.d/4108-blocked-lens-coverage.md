## Fixes

### Pipelines and graphs

- A review lens whose environment cannot be built is now reported as unavailable evidence instead of ending the graph as `blocked_environment` with a same-route retry and a chair pause. Its sealed stop is fenced so no retry or pause sinks the panel, its cause and handoff note name the gap in the panel result and warnings, and the panel settles on `min_successful` with the lenses that did finish. A panel whose only gap is a named blocked environment keeps complete findings from finished lenses available and an inconclusive verdict so a delivery review can post the gap report. Blocking findings and hard coverage floors still refuse. (#4108)
- Ignored adapter capability entries identify their configuration layer and key in the warning's structured source.
