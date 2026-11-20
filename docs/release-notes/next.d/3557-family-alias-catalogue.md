## Fixes
### Accounts, quota and routing

- Family aliases such as `claude/sonnet` now resolve through the effective model catalogue to their latest matching exact model, so graph launch no longer warns that the alias is not in the catalogue. Launch uses that exact model and keeps the requested alias as `requested_model` in the receipt, so a provider that reports the exact model is no longer judged an identity mismatch. Unknown or ambiguous aliases still warn. (#3557)
