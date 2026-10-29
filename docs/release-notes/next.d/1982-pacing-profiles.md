## Additions
### Accounts, quota and routing

- Pacing policies, window reserves and deadlines can be set per account, including pacing against another account's reset and temporary overrides that expire. They also apply to free models (#1982).
- A monthly `day 31` deadline clamps to shorter months, and `daily` is accepted as a window selector (#1982).
