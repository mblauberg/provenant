## Additions
### Pipelines and graphs

- Preset parameters can be typed tables with `type`, `default`, `min`, `max`, `enum` and `doc`; a refused value names the parameter and the layer it came from. A `[pattern]` table records level, summary, citations, cost and when to select a preset. `preset explain NAME` shows both without creating state, `preset list` shows level, summary and cost, and `risk_tier` and `subject` flow from a parent to a child that declares them. The new `scope-then-run` preset scouts and scopes before implementing (#2209).
