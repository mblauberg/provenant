## Behaviour changes
### Pipelines and graphs

- Writer, repairer, refactor and review roles default to running only targeted local checks, leaving full suites to CI on the pull request and combined checks to the batch head. Projects can override the policy in `.provenant/roles.toml` or via the `local_check_policy` preset parameter (#2758).
