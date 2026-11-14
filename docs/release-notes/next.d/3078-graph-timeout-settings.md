## Breaking changes
### Pipelines and graphs

- **`domains.fabric.graph.writer_launch_seconds` is removed.** Set `domains.fabric.graph.launch_wait_seconds` for general launch/wait work and `domains.fabric.graph.writer_provision_seconds` for each writer provisioning; both default to 55 seconds and report their configuration source. (#3078)
