## Additions
### Telemetry and providers

- `provenant telemetry prices refresh` fetches OpenRouter and LiteLLM prices and writes a proposed diff with source URLs and fetch time, reporting cross-source discrepancies without resolving them; it never edits the price sheet. Price rates take optional `effective_from` and `effective_to` dates, and `telemetry stats` flags a sheet reviewed longer ago than `max_review_age_days` (#2241).
