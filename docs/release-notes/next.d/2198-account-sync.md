## Behaviour changes

- `install`, upgrades and `accounts register` now keep Provenant's managed skills, MCP entry and hooks identical in every registered Claude and Codex account directory. Registration installs into the new account straight away when an install exists.
- Hooks are one choice per harness: a target with none yet, such as a new account, takes the hooks that another target of the harness owns. A deleted managed hook is restored on the next sync, and edited hooks are still never overwritten.
- `provenant accounts sync [--dry-run]` repairs drift in every registered account.
- `check install` names the drifted account and adds `unsynced_account`, `missing_account_directory`, `unusable_account_directory` and `account_missing_entry` findings. `accounts list` and `hosts doctor` report a sync state per account.
