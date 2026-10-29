## Additions

- Quota refreshes now keep a history. Each account appends a reading to `quota/history/<account_key>.<adapter>.jsonl` when its windows, plan, credit balance or reset credits differ from the last reading, marking `reset` and `plan.change`. The file rotates under `domains.quota.history_max_bytes` (4 MiB by default). Nothing is backfilled, so history starts at this release.
- `provenant quota history [--account] [--since] [--until] [--limit]` lists readings, with `--json` carrying `schema_version` and the plan price in force at each reading.
- Accounts take a `plans` table (`name`, `price`, `currency` defaulting to USD, `notional_usd` for free plans, `effective_from`, any number of entries) for plan prices over time, and an optional `directories` list of further config directories for the account. Part of #2225.
