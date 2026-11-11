## Fixes
### Pipelines and graphs

- Several launches submitted at once in one repository no longer fail with `io` ("graph input could not be processed"): preparing project scratch waits up to 30 seconds for the repository exclude lock instead of spending the launch's `--wait`, and a lock that stays held is reported with its cause (#3232).
