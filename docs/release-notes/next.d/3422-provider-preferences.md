## Behaviour changes

### Telemetry and providers

- Codex writers can select clean configuration with `providers.codex.clean_config`;
  read-only launches retain their configuration boundary. Provider capability
  failures can use eligible fallback routes, while local diagnostics and task
  briefs preserve their text. Advisory GitHub deny-list matches warn and proceed.
  (#3422)
