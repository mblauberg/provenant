## Fixes
### Pipelines and graphs

- `stop` records cancellation while a graph launch holds its journal lock, reports the request while acknowledgement is pending, and prevents further launches within the cancelled scope once acknowledged. Recovery retains route and account pins and closes linked asks on cancellation. (#3078, #3254)
