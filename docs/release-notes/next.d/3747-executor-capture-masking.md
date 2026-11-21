## Fixes

### Telemetry and providers

- Every executor capture now states its purpose at construction as a required typed choice: an exact capture keeps raw bytes for parsing, hashing or content, while a diagnostic capture is masked on the stream with local-disclosure redaction ahead of any bound. There is no default and no opt-out allowlist, so a byte-exact read (Git diffs and logs, seed identity, JSON envelopes) can no longer be silently rewritten to `[REDACTED]`. Where an exact capture's text is shown in an error or a log, it is masked at that disclosure point instead (#3747)
