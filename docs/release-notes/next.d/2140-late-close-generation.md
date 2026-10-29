## Fixes
### Git and safety
Attempts dispatched after a PR was merged or closed outside `land` and `pr status` now move to the next branch generation when the stale binding is swept, so they link to the new PR only. `pr contributions` reports `reassigned`.
