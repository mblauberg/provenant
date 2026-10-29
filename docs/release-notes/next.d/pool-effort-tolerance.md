## Behaviour changes
### Accounts, quota and routing

- A routing pool member that lists an effort its catalogue model no longer supports no longer invalidates the configuration. The unsupported efforts are dropped when the pool resolves, a default `effort` that is dropped falls back to none, and dispatch prints a warning naming the `domains.routing.pools.<name>[<index>].efforts` field. Malformed, duplicate and more than 16 efforts are still refused.
