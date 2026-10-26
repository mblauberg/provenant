## Breaking changes

- Fabric graph routes now accept optional `account` in `route` and every
  `routes` entry. Graph checkpoint envelopes under
  `fabric/graphs/<project>/graph-<id>.json` now require `execution.document`
  and `execution.directory`; their digest covers those host-only inputs.
  Existing checkpoints cannot be read or resumed by the new release. Finish
  active graphs with the previous release and archive their checkpoints
  before upgrading (#1712, #1713).
