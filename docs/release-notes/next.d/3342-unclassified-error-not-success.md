## Fixes
### Telemetry and providers
- An unclassified provider error now settles as `Unclassified` even after assistant text and exit 0; only a clean stream with no error settles as `Succeeded`. (#3342)
