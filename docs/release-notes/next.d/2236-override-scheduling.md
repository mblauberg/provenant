## Additions
### Accounts, quota and routing
`provenant route override set` records a pacing override with `--starts`, `--until`, repeatable `--window [DAYS@]START-END` (such as `mon-fri@23-7`) and `--tz`, and echoes the recorded rule and any rule it replaced. An override no longer needs an expiry. `route override list` shows every pacing override and scheduled model policy rule with its active state and time remaining, and `route override clear` removes every override or one scope's. Expired overrides are pruned on the next change (#2236).
