## Fixes

### Accounts, quota and routing

- Protect dedicated account directories and resolve shared roots to declared and Provenant-owned credential files, so a shared home does not block unrelated lane roots; name the account when its store cannot be resolved. #3392

- Classify built-in stores alongside host and account declarations, retain whole-store protection for relocated Codex homes, and use the GitHub CLI file inventory when its configuration shares a lane root. #3392
