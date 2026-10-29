## Additions
### Chair, inbox and console

- `provenant steer` and `provenant chair message` queue a bounded, idempotent control message, and `provenant help --all --json` publishes a `provider_controls` capability matrix per adapter and launch kind. Chair messages go to the exact registered session as Fabric mail; steering refuses with `control_unsupported` because no lane adapter offers live control. Neither can approve a gate, merge or landing. `fabric_inbox` gains `steer` and `chair_message` (#2311).
