## Behaviour changes
### Pipelines and graphs

- Graph and run retries share the existing `settlement_retries` allowance (default 2). Preset and node `retry.retries` overrides take precedence; full graph status shows the effective value and origin, retained across replay (#2697).
