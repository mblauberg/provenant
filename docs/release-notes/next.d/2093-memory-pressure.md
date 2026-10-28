## Behaviour changes

- Defer lane admission at macOS warning or critical memory pressure and limit host-wide writer build concurrency. Configure the build slots and default Cargo jobs per host (#2093).
- Release stale writer slots when their recorded guardian is gone or their boot has ended, and report pressure and slot refusals.
