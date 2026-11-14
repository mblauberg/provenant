## Fixes
### Accounts, quota and routing
- Repositories can declare confidential inputs in `.provenant/config.toml` under `[launch_policy]`, using `confidential = true` or `protected_paths = ["private"]`. Launch records show the classification source, confidential work excludes training-enabled and unknown-treatment models, and recovery retains the accepted policy.
- Sandbox admission warns by default when a child lowers a user-enabled parent's sandbox. Set `lanes.sandbox_floor` to `strict` to retain it through continuation and fallback, or `off` to disable the advisory. Sandbox defaults remain off.
