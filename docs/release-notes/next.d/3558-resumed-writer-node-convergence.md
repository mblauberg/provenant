## Fixes

### Pipelines and graphs

- A graph resumed after a supervisor restart against lowered host structural limits no longer leaves a succeeded writer's node in `running`: the terminal attempt is still observed under the saved pause, so its node settles and `head` reports the writer's final commit from the bound branch instead of the base. The structural pause is preserved while any node remains live. (#3558)
