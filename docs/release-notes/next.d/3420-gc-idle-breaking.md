## Breaking changes

### Lanes and settlement

- Replace `gc --idle-days N` with `gc --idle Nd`, and replace `gc.idle_days` with a duration such as `gc.idle = "6h"`; the idle default is now six hours. (#3420)
