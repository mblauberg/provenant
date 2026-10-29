## Additions
### Pipelines and graphs

- Presets can fan a role out over a list with `for_each` and read `${item.name}`, append a bundled prompt with `prompt_file`, and require a model family distinct from the writer's with `distinct_from = "writer"`. `run --preset` takes `--params-file` (#2206).
- A graph that reads the integration branch pins its head, includes it in the graph ID and stops before synthesis if the branch moves. Review lenses default to the #2187 set and take their tiers from the routing review shape chosen by risk (#2206).
