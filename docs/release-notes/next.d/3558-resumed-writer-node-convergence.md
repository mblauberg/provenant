## Fixes

### Pipelines and graphs

- A graph resumed after a supervisor restart against lowered host structural limits no longer leaves a succeeded writer's node in `running`: the terminal attempt is still observed under the saved pause, so its node settles and `head` reports the writer's final commit from the bound branch instead of the base. The pause still admits no new launch: attempts the host never accepted are not started. It stays while any node is live and the graph is over the lowered limit, and clears when the graph fully settles or the limit is raised. (#3558)
