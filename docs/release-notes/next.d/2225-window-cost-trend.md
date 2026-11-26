## Additions

### Telemetry and providers

- `provenant telemetry value` now bases its completed-window trend on API-equivalent cost per full window, with the change in microdollars. Window cost uses ledger activity through the quota observation paired with its utilisation; cycles without usable bounds or fully priced usage make the trend unavailable. (#2225)
