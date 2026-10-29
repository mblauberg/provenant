## Behaviour changes
### Pipelines and graphs

- Projects can opt into stable work-item assignments across preset control and candidate arms, record the experiment seed and arm on graphs, run shadow assignments on the control preset, and inspect stratified offline outcome comparisons with `telemetry experiments show` (#2210).
- Automatic preset selection accepts `--task-id`, so a work item can take part in an experiment. `--preset` still cannot be combined with `--task-id`.
