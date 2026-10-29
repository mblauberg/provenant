## Behaviour changes
### Accounts, quota and routing

- `install`, upgrades and `accounts register` now keep Provenant's hooks, skills, MCP entry and shared Claude settings identical in every registered Claude and Codex account (#2198).
- `[domains.accounts.shared_settings]` selects which `settings.json` keys are shared. A value edited in one account is reported as `settings_conflict` and kept, and `provenant accounts sync --adopt ACCOUNT` or `--force-settings` resolves it (#2198).
- `provenant accounts sync [--dry-run]` repairs drift in every registered account, and `check install`, `accounts list` and `hosts doctor` report each account's sync state (#2198).
- `uninstall` no longer fails on a configuration defect; it warns and removes the recorded targets (#2198).
