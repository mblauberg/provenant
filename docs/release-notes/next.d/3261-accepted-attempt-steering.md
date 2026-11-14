## Fixes
### Pipelines and graphs

- `steer` can address an accepted live attempt before its graph checkpoint records the attempt reference, using verified local or authenticated peer ownership. Unsupported controls refuse with the `retry` alternative. (#3078, #3261)
