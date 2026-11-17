## Fixes

### Telemetry and providers

- Provider JSON output retains its structure while masking decoded secrets, including escaped values and strings larger than capture frames. Incomplete, invalid and oversized provider frames report their line number and cannot settle successfully. Command output retains tail diagnostics even when a line exceeds 1 MiB. (#3395)
