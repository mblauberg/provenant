## Fixes
### Lanes and settlement

- Completion cleanup keeps requests and retry state together, recovers dead workers and preserves fresh requeues after exhaustion. Settlement queues cleanup without waiting for a busy dispatch lock (#3050).
