## Fixes

### Lanes and verification

- Lane writer preambles now render the project's valid `verify.lane` declarations (tier, argv, scope and source) as the authoritative verification guidance, and the launch receipt records those same declarations and their source. With no usable declaration the resolved scope or repository formatter and declared pre-commit hooks remain in force, and invalid entries warn rather than block the launch. Worktree provisioning runs `build.prepare` once per declaration digest; a lane preparation failure warns and stays retryable while a slot failure still fails its job. (#4069)
- The lane Git wrapper unsets `LEFTHOOK` and warns then refuses `--no-verify` and `core.hooksPath` overrides, because hooks are part of commit and push. A Git command whose only remaining option takes a value (for example a bare `-c`) now reaches Git unchanged instead of aborting the wrapper. (#4069)
