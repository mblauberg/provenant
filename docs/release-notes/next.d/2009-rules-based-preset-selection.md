## Additions
### Pipelines and graphs

Runs without an explicit model, route or preset select a pipeline from bundled
rules and optional repository overrides. Readiness routes underspecified work
through `scope-then-run`, and graph attempt context records the selected
preset and selection source.
