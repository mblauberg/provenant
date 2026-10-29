## Breaking changes
### Pipelines and graphs

- Presets all lower through one stage path. `single`, `chain`, `fan-out` and `fan-out-synthesise` are now bundled stage presets, and the `structure` and `slots` forms are removed. `--set` paths are replaced by stage addressing: `slots.first.parameters.route=X` becomes `first.route=X`, `slots.NAME.preset=P` is set by editing the stage's `preset`, and a custom preset composes presets with `stages = [{ id = "first", preset = "fan-out" }]`. `parameters.route` and `parameters.width` are unchanged. A stage that runs a preset is now a composite, so its children are `first--branch-1` rather than `first-branch-1`. `implement-with-lens` is renamed `generator-evaluator`, and the `risk` parameter is `risk_tier` (#2215).
