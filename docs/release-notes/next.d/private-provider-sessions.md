## Breaking changes
### Telemetry and providers

- **Attempts from before this release using Cursor, OpenCode or read-only Claude cannot resume.** These adapters now require attempt-private session state. Start a new lane.
