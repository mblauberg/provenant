## Additions
### Lanes and settlement

- A manifest task takes a `network` field (`on` or `off`) that overrides `--network` for that task, and `--route ADAPTER/NAME` names a model-less command lane without a manifest, for example `--route command/fmt-check --network off` (#2276).
