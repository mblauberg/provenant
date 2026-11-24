## Fixes

### Lanes and settlement

- A writer launched from a template (`provenant run implement --branch` or `--base`) no longer takes the read-only default source: a primary checkout behind its upstream neither warns nor pins the launch, and the launch identity comes from the writer's base. (#3870)
