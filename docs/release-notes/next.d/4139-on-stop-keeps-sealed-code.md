## Fixes
### Pipelines and graphs
- When an explicit `on_stop` override suppresses a same-route sealed stop, a failing node now reports the sealed cause's advance code instead of the provider outcome's label: a lost lane token is no longer reported as `auth_required`. (#4139)
