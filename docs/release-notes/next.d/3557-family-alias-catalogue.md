## Fixes
### Accounts, quota and routing

- Family aliases such as `claude/sonnet` now resolve through the effective model catalogue to their latest matching exact model, so graph launch no longer warns that the alias is not in the catalogue. Unknown or ambiguous aliases still warn. (#3557)
