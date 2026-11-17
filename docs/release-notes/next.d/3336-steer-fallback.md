## Behaviour changes

### Lanes and settlement

- When a steered provider session is unavailable, settlement warns with the setting source and starts a fresh continuation carrying the accepted brief, prior handoff and queued steer text; set domains.fabric.steer.fallback = "refuse" to disable the fallback. (#3336)
