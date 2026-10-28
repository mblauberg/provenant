## Additions

- Update idle provider CLIs on quota housekeeping events, with per-provider opt-out, command overrides and a configurable 24-hour default interval. Shared lane leases fence native updates, including fallback providers. Activity and the next lane's availability record retain old and new versions; failed updates wait until the next interval.
