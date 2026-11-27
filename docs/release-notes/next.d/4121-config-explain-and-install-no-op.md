## Fixes

### Config and install

- `config explain KEY` reports string-valued settings from `config.toml` and rules from a project's `.provenant/policy.toml` as candidates with their layer and origin. (#4121)
- Reinstalling an unchanged target is reported as a no-op from the current receipt under the install lock, so owned files are not rewritten. (#4121)
