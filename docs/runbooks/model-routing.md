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

Use `provenant help routes` to inspect configured task classes and current
route health. `AGENT_FABRIC_STATE_ROOT` relocates both health and cooldown
records; tests may override the health file with
`AGENT_FABRIC_ROUTE_HEALTH_PATH`.
