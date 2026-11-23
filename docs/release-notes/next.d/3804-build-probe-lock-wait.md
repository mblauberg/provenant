## Fixes
### Lanes and settlement

- A build waiter no longer fails admission when another lane's liveness scan holds its ticket lock for the moment before its own flock lands; the waiter waits out that transient hold instead of reporting a zero-timeout refusal. (#3804)
