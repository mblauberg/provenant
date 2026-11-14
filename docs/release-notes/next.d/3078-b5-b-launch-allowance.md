## Fixes
### Pipelines and graphs

- `run` and the scheduler's `tick` use `domains.fabric.graph.launch_wait_seconds` for general launch and wait work. Writer provisioning, including a busy writer registry, uses its independent `domains.fabric.graph.writer_provision_seconds` allowance. `status NAME` and `stop NAME` now reach a graph whose record is unreadable, so the retirement command that status names works. (#3078)
