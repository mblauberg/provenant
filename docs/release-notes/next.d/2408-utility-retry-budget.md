## Fixes

- Writer launch now treats its Git budget as the bound for the whole upstream config write, not a fresh timeout per retry. The lock-contention retry loop computes one deadline from the budget, gives each attempt only the time still left, stops retrying when that time cannot cover another backoff and attempt, and its timeout error reports the attempts it counted; previously five contending attempts under the writer's 5 s budget could take about 25 s to set one config key ([#2408](https://github.com/mblauberg/provenant/issues/2408)).
