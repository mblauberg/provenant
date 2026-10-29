## Additions

- Idle provider CLIs update automatically, triggered by quota events and run by a one-shot background worker. Updates can be turned off per provider, the command can be overridden as an argument vector, the interval defaults to 24 hours and the updater timeout to 300 seconds, and both are configurable (#1977).
- A provider is never updated while its lanes run. A busy provider does not delay lanes that have other candidates. When every candidate is busy, lane preparation waits at most 30 seconds in total and honours cancellation (#1977).
- A timed-out updater is killed with its process group. Failed or interrupted updates back off for the interval. Activity and the next lane's availability record the old and new versions (#1977).
