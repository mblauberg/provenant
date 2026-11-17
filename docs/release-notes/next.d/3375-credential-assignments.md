## Fixes

### Telemetry and providers

- An unclassified provider error keeps its bounded provider message in the reason, alongside the diagnostic. Credential assignments such as `password=…`, `token:…`, `api_key=…` and `Authorization: Basic …` are redacted before the message reaches status or output. (#3375)
