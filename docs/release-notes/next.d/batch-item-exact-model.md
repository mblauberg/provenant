## Fixes
### Pipelines and graphs

- A batch item whose `model` contains a slash now keeps the full adapter/model id and routes exactly like `run implement --model`, instead of dropping its adapter's first segment during preparation.
