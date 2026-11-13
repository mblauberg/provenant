## Fixes
### Pipelines and graphs

- `run` and the scheduler's `tick` bound a launch by `domains.fabric.graph.writer_launch_seconds`, so a busy writer registry or provider update is reported within the configured allowance instead of a fixed 55 seconds. `status NAME` and `stop NAME` now reach a graph whose record is unreadable, so the retirement command that status names works. (#3078)
