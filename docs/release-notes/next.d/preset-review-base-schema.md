## Fixes
### Pipelines and graphs

- `run --preset review-council` with a `base` no longer fails on submit with `preset_graph: invalid input`, a regression in 1.5.5 that also hit other presets with a review stage, such as `generator-evaluator` with a fixed review shape. Fabric refusals of invalid input now name the field or reason, for example `invalid input: graph schema version`.
