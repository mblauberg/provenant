## Fixes

### Pipelines and graphs

- A graph resumed after a supervisor restart against lowered host structural limits no longer leaves a succeeded writer's node in `running`: the terminal attempt is still observed under the saved pause, so its node settles and `head` reports the writer's final commit from the bound branch instead of the base. The structural pause is preserved while the graph is over the lowered limit, including after attempts settle, and clears only once the limit is raised. (#3558)
