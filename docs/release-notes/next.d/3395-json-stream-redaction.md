## Fixes

### Telemetry and providers

- Provider JSON output retains its structure while masking decoded secrets, including escaped values and strings larger than capture frames. Incomplete, invalid and oversized provider frames report their line number and cannot settle successfully. Raw command output and malformed session history mask escaped credentials while preserving trailing diagnostics and original non-secret bytes. Valid JSON retains its original escapes, and replay allows bounded mask growth. Command output retains tail diagnostics even when a line exceeds 1 MiB. (#3395)
