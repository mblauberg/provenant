## Behaviour changes
### Accounts, quota and routing

- A routing pool member that lists an effort its catalogue model no longer supports no longer invalidates the configuration. The unsupported efforts are dropped when the pool resolves, a default `effort` that is dropped falls back to none, and dispatch, `check config` and `hosts doctor` report warnings naming the pool, effort, model and config source of each value. These diagnostics do not fail checks. Malformed, duplicate and more than 16 efforts are still refused.
