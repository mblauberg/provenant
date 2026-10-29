## Additions
### Accounts, quota and routing
- `[domains.quota.polling_intervals.<adapter>]` sets `timer_seconds` and/or `panel_seconds` for each registered adapter. Omitted values keep the provider default. Quota ticks, `quota show` staleness and routing admission use the configured values. See the [configuration runbook](../../runbooks/configuration.md#quota-polling-intervals) (#2272).
