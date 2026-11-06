## Breaking changes
### Telemetry and providers

- Provenant no longer reads OpenCode's older `message` table, so OpenCode versions whose database lacks `session_message` lose model identity on lane records. Upgrade OpenCode; 2.2.5 is unaffected.
