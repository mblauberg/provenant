## Fixes
### Telemetry and providers

- An OpenCode route whose effort the model does not offer is refused at
  resolution with the valid variants listed, and the provider's
  "Variant unavailable" failure is classified as a route refusal with guidance
  instead of a model outage. (#3707)
