## Fixes

### Fabric

`provenant run implement` no longer emits the `unbounded_brief_junior_writer` advisory for a junior writer without a plan input, write scope or explicit `plan` parameter, and the `domains.fabric.implement.warn_unbounded_junior` knob is removed. Implement starts a junior writer directly by default, so the warning contradicted the documented default and fired on every unplanned brief. No compatibility alias is kept: a host config that still names `warn_unbounded_junior` now receives the config loader's ordinary unknown-field warning, so remove that key. (#3494)
