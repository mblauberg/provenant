## Fixes

### Accounts, quota and routing

- `accounts register` accepts nested agy HOME roots while refusing shared state or credential stores. Pools can use both the local login and registered accounts; recorded agy refusals expire within five minutes so stale reset estimates cannot block working models. #3423
