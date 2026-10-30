## Fixes
### Accounts, quota and routing

- `provenant config migrate` (with `--dry-run` and `--json`) rewrites the retired `starts_unix_seconds`, `expires_unix_seconds` and `windows` routing keys to `schedule` after saving a timestamped backup, and drops pacing overrides that have expired. The load warning and the refusal of config writes such as `provenant accounts register` now name it as the repair.
- An agy quota probe that finds a different Google account signed in than the registered one reports `account_mismatch` and the `provenant accounts register --adapter agy --directory <dir> <label>` repair, instead of looking like a sign-out. The account stays `auth_required` for dispatch until it is re-registered.
- `provenant run --mode read-only --worktree` is refused as a mismatched mode instead of being turned into a writer run, which made read-only models such as `agy/gemini-3.8-flash` fail with `model policy restricts this route to read-only and review work`.
