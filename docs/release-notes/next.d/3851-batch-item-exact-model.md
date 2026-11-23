## Fixes
### Pipelines and graphs

- A batch item whose `model` contains a slash now keeps the full adapter/model id and routes exactly like `run implement --model`, instead of dropping its adapter's first segment during preparation. Graph routes now carry that one full catalogue id from admission through preparation, so exact-model tiers and node route tables cannot be split or re-parsed, and a declared route equal to a ladder hand-off is not offered twice. (#3851)
