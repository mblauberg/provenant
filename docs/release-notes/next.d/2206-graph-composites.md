## Breaking changes

`review-council` takes a `lenses` list instead of `correctness_tier`,
`simplicity_tier` and `risk_tier`, and `big-batch` takes a `lanes` list instead
of `lane_one_*` and `lane_two_*`. The role fields `route` and
`required_verdict` are gone; use `tier` or `routes`. Graph and preset
`read_roots` must lie inside the project or in the new host allow-list
`[domains.fabric.graph] allowed_read_roots`.

## Added

Presets can fan a role out over a list with `for_each` and read `${item.name}`.
A role can append a bundled prompt with `prompt_file` and require a model
family distinct from the writer's with `distinct_from = "writer"`. `run
--preset` takes `--params-file`. The bundled presets are `scout-swarm`,
`implement-with-lens`, `review-council` and `big-batch`. Review lenses default
to the #2187 set, take their tiers from the routing review shape chosen by
risk, and state the base, the spec and whole-repository read access. Refs #2187.
A graph that reads the integration branch pins its head, includes it in the
graph ID and stops before synthesis if the branch moves. Refs #2194.
