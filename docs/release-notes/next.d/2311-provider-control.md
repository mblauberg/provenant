## Additions
### Chair, inbox and console

- `provenant steer` and `provenant chair message` queue a bounded, idempotent control message, and `provenant help --all --json` publishes a `provider_controls` capability matrix per adapter and launch kind. Chair messages go to the exact registered session as Fabric mail; steering refuses with `control_unsupported` because no lane adapter offers live control. Neither can approve a gate, merge or landing. `fabric_inbox` gains `steer` and `chair_message` (#2311).
- `provenant fabric mail read` and `mark-read` take `--session` (MCP `fabric_mail` takes `session`) to read a chair session's control mail with its own cursor, so sessions that share an agent label cannot read or consume each other's messages (#2311).
