# Model routing

`config/model-routing.json` defines the supported task classes and their exact
route order. Each `task_class_routes` entry binds an alias floor, effort, role
and `models` map. Its keys are adapter IDs and values are ordered lists of
exact model registry IDs. The dispatcher checks runtime capability evidence
and uses the first available model in that order; an unsupported exact model
fails closed.

The instance overlay at `~/.agents/config/model-routing.json` is read-only to
the product and may override a class's `models` and `effort`, subject to the
class's minimum effort and role policy. Object fields merge by name and list
values replace the product list. Keep each adapter list to model IDs
registered for that adapter and supporting the configured effort.

Task classes include `implementation`, `ui-taste`, `screenshots`, `review`,
`second-opinion`, `research`, `bulk`, and `critical-review`. The review classes
may have one configured alternate. After an empty or failed outcome, a resolve
for that class selects the next configured route with no recent failure,
instead of repeating the failed model. A successful later outcome clears that
route's failed state for selection.

Fabric records the 20 latest outcomes per adapter, model and task class in
`~/.local/state/agent-harness/fabric/route-health.json`. Outcomes distinguish
`ok`, `failed`, `empty_output`, `cancelled`, `rate_limited` and `usage_limited`.
The health view also includes the active cooldown from `cooldowns.json`; expired
cooldown markers are ignored even when an old marker remains on disk.

Global route pools (`routes`: `strong`, `bulk`, `design`, `writing`) sit
beside the task classes. Each entry names `adapter/model`, a `weight` (`high`,
`normal`, `sparing`, `off` or a number) and an optional two-value `effort`
band; `model_traits` adds traits beyond the OpenCode `free_pattern`, and
`route_synonyms` maps task classes and tier aliases onto a pool. In the overlay a
`routes.<name>` list merges by `model`: listed entries come first in overlay
order and take the overlay's fields, and unlisted product entries follow. So
`[{"model": "claude/claude-opus-5-5", "weight": "off"}]` switches one model off
without restating the rest. `provenant routes` (or `routes --json`) prints each
pool with live availability; the rotation cursor lives in
`route-rotation.json` under the same state root. Overlay entries match product
entries by canonical model, so a spelling alias such as `codex/gpt-6-sol`
switches off the seeded `codex/gpt-6.1-sol`. `refresh-routing` merges a pool
per entry and per field, so an instance edit and a product edit to the same
pool both survive. Adapters with `latest_aliases` also resolve version-free names such as `gpt-sol` to the
newest model (see `skills/orchestrate/references/routing-and-tiers.md`).

`provenant help routes` is `provenant routes --health`: every pool with live
availability, then the configured task classes and current route health. `AGENT_FABRIC_STATE_ROOT` relocates both health and cooldown
records; tests may override the health file with
`AGENT_FABRIC_ROUTE_HEALTH_PATH`.
