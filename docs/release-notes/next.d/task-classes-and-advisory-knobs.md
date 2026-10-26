## Breaking changes

- Task classes are removed. `config show` no longer takes `--task-class`, the
  knob registry's presets no longer carry `task_classes`, and a Fabric route
  no longer accepts `task_class`: `fabric_submit` rejects it as invalid input,
  and the field is gone from its schema. A route that named only a task class
  now names no route, so give it a `route` or `model`. New dispatch profiles
  record no `task_class`; old runs stay readable, including requests and
  attempts sealed with that field. Upgrade every host together (#1571).
- The eight advisory knobs are removed from the registry, since no code read
  them: `effort.shift`, `context.ceiling_tokens`, `repair.max_cycles`,
  `council.junior_size`, `routing.pool_mode`, `writing.final_polish`,
  `context.summary_tokens` and `quota.pace_tolerance`. Their defaults are now
  chair judgement in `skills/orchestrate/SKILL.md`. A `[knobs]` entry
  or `--knob` override naming one is refused as an unknown key, and
  `config set` and `config unset` refuse them too, so delete any such line
  from `config.toml` by hand. The `economy` and `rigorous` presets keep only
  their wired knobs, and with no nullable knob left, `config set` no longer
  accepts `null` (#1571).
- Routing config lives only under `[domains.routing]`; the root `[routing]`
  alias is removed and a root `[routing]` table is refused as an unknown key.
  Move any such table under `[domains.routing]`. `route set`, `route remove`,
  `route reset` and `route pacing` now write there (#1571).
- `config set` and `config unset` use the same editor as the `route` verbs:
  they refuse to rewrite a `config.toml` that does not already validate, and
  report a domain error before writing rather than after. Route edits accept
  inline domain tables as well as table headers (#1571).
