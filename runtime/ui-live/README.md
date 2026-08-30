# UI live runtime

This private runtime contains the stateful live-iteration implementation used
by `ui-ux-design`. It is not a separate skill, npm package, workspace, service,
or install lifecycle. The skill keeps its stable `scripts/live*.mjs` commands
as thin compatibility entry points.

The runtime is resolved from an explicit product root or the physical skill
checkout. It uses the target project only for authorised source work and
project-local live state; Fabric registration, seats, and state never locate
the runtime itself.
