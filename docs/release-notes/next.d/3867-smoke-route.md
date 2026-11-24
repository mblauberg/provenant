## Fixes

### Pipelines and graphs

- `provenant run smoke --route <adapter/model>` and `provenant upgrade --verify --route <adapter/model>` accept a short or full catalogue model id instead of refusing it as an invalid alias, and relay controls no longer rewrite an exact-model route. A graph refused at admission now names the node and the validator's reason in `dispatch_refused`. (#3867)
