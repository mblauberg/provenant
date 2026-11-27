## Fixes

### Error contract

- A host clock failure during a run-store call is reported with the `TIME` code and its leaf cause instead of failing to convert into the run-store error contract. (#4121)
