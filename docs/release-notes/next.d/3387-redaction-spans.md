## Fixes

### Telemetry and providers

- Provider output keeps quoted credentials masked when they overlap known secrets, masks credential suffixes after `[REDACTED]` followed by a backslash, and continues capturing output after large redactions. (#3387, #3386, #3400)
