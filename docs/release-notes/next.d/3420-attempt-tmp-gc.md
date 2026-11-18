## Behaviour changes

### Lanes and settlement

- Attempt scratch remains available after release and is collected as soon as its owning work is finished; merge cleanup is targeted, while low-space launches trigger a single-flight background sweep. (#3420)

## Breaking changes

### Lanes and settlement

- Replace `gc --idle-days N` with `gc --idle Nd`, and replace `gc.idle_days` with a duration such as `gc.idle = "6h"`; the idle default is now six hours. (#3420)
