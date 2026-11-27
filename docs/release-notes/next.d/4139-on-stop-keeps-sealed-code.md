## Fixes
### Pipelines and graphs
- When an explicit `on_stop` override suppresses a same-route sealed stop, a failing node now reports the sealed cause's advance code instead of the provider outcome's label: a lost lane token is no longer reported as `auth_required`. (#4139)

- A sealed stop now also keeps its advance code when fallback is denied or a route is pinned; the accompanying note still explains the refusal. (#4139)

- Run receipts and refusal details continue to show preparation refusals when their advance code names a specific sealed cause. (#4139)
