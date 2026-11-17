## Breaking changes

### Lanes and settlement

- Stop requests now live in `stop-intent.json` and guardian ownership in `guardian-lock.json`. A wall-deadline, budget or steer stop that an earlier release recorded for a running lane is ignored after the upgrade, and such lanes get no automatic guardian-loss recovery. Let running lanes settle, or stop them, before upgrading. (#3336, #3335)
- A steer in flight across the upgrade may refuse once, because the session identity digest changed; steer again after the upgrade. (#3359)
