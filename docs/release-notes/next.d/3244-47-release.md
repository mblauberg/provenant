## Fixes
### Accounts, quota and routing

- Repositories can declare confidential inputs in `.provenant/config.toml` under `[launch_policy]`, using `confidential = true` or `protected_paths = ["private"]`. Launch records show the classification source, confidential work excludes training-enabled and unknown-treatment models, and recovery retains the accepted policy. (#3244)
