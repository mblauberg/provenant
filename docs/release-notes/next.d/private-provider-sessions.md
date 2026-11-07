## Behaviour changes
### Telemetry and providers

- Cursor, OpenCode and Claude lane sessions now use attempt-private provider state. Older attempts whose sessions were stored on the host cannot be resumed and report that retained continuation is missing or expired.
