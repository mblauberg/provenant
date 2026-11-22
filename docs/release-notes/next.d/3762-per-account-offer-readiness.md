## Fixes

### Accounts, quota and routing

- An agy account whose recorded offer list lacks the requested model is no longer listed as ready by `provenant route resolve`, and an unpinned resolution does not select it when its usage probe has not run. The exclusion names the account, model and recorded offers. An explicit `--account` pin is kept with a warning and still attempted. The synthetic `local` quota row no longer appears as a route candidate once an adapter has registered accounts (#3762)
