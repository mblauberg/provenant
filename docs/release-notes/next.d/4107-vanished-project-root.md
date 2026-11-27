## Fixes

### Scheduled host work

- A registered project whose checkout has vanished no longer fails settlement and `forge_events` on every tick. When the verified primary checkout is gone — even though its separate Git common directory survives — both tasks report it once with `PROJECT_ROOT_VANISHED` and the `provenant gc --apply` unregister command, then skip it; other projects keep their `ok` task state. `provenant gc --apply` now retires such a binding once its record has been idle for `gc.idle`, matching the advertised remedy. (#4107)
