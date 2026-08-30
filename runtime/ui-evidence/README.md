# UI evidence runtime

This private runtime contains the deterministic UI anti-pattern detector used
by `ui-ux-design`. It is not a separate skill, npm package, workspace, or
service. The skill remains the only user-facing owner and keeps
`skills/ui-ux-design/scripts/detect.mjs` as its stable command.

The runtime is resolved from an explicit product root or from the physical
source checkout. It does not use the target project's current directory,
`node_modules`, Fabric registration, seats, or state.
