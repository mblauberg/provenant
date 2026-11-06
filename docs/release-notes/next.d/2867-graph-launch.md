## Behaviour changes
### Pipelines and graphs

- `run` starts a graph for plain tasks, templates and presets. Inside a lane, `run` and `fabric_run` add children to the parent's graph; `--use` selects a template (#2867).
