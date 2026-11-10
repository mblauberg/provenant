## Breaking changes
### Telemetry and providers

- **Cost and plan value now come from the usage ledger, in two commands.** `provenant telemetry cost [TARGET] [--by route|account|model|graph|node]` reports API-equivalent cost, and `provenant telemetry value` compares each account's usage with its plan price and quota readings. Both state unpriced and unattributed attempts instead of counting them as zero. `telemetry stats` no longer reports cost (including route outcome cost per success and the chair session cost) or `plan_value`, `telemetry mileage` no longer reports `effective` rates, and `quota usage` reports no cost; use the new commands. (#3165)
