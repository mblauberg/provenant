## Additions
### Telemetry and providers

- `provenant telemetry mileage` reports the API-equivalent cost of a quota percentage point and a full window from quota history and ingested usage, splitting runs at resets, drops and plan changes and flagging capacity step changes only with enough priced segments. It also reports potential and realised effective rates per token bucket, with the latest used percentage, sample size, priced coverage and a lower-bound note on every effective report, since ingest coverage is not checked (#2240).
