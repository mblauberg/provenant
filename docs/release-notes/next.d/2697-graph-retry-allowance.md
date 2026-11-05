## Behaviour changes
### Pipelines and graphs

- Graph nodes seal the effective settlement retry allowance and its origin. Preset and node `retry.retries` overrides take precedence; verified pre-launch preparation fallbacks do not consume it (#2697).
