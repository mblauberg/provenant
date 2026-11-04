## Breaking changes
### Pipelines and graphs

- **Preset explanations use `admission`, `inspection` and `pending`.** Update consumers of `execution` or `validation_error`. Explanations and submission now use the same static admission checks (#2821).
