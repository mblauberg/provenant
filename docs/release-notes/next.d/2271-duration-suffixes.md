## Additions
### Accounts, quota and routing

- Route pacing `--starts` and `--until` accept weeks and compound durations such as `1w` and `1h30m`, matching the duration suffixes used for provider reset messages and quota windows (#2271).
- Provider quota windows and reset messages recognise additional duration suffixes, including compound windows, seconds and weeks (#2271).
