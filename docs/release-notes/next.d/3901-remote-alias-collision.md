## Fixes
### Git and safety
- A writer branch is no longer refused because its name only collided with a remote-tracking ref under a remote alias that does not exist. The collision check now splits `refs/remotes/` only at the repository's configured remote names, longest first, so a configured remote containing `/` still matches while a stray ref such as `refs/remotes/origin/w1ci/smoke` no longer invents `origin/w1ci`. The refusal now names the colliding ref, with display-unsafe characters withheld. (#3901)
