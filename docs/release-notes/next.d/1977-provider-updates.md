## Additions

- Update idle provider CLIs on quota housekeeping events, with per-provider opt-out, direct argv command overrides, a configurable 24-hour default interval and a configurable 300-second updater timeout. Shared lane leases fence native updates, including fallback providers. Busy providers do not delay lanes using other candidates; preparation waits at most 30 seconds total only when all candidates are busy.
- Housekeeping retains exclusive updater leases and kills the updater process group on timeout. Failed and interrupted updates are recorded and retried after the interval. Activity and the next lane's availability record retain old and new versions. Provider background processes retain shared leases and defer updates until they exit.
