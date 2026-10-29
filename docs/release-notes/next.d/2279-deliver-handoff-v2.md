## Behaviour changes
### Pipelines and graphs

- The `remaining` input selector now carries the `remaining[]` items of the node's handoff v2 block instead of the closing window of a clipped report, and the `deliver` repair briefs read it from the writer and the previous repair. A provider that emits no handoff gets no tail window (#2279).
