## Fixes

### Lanes and settlement

- Writer lanes no longer start with a cold Cargo target when the host is busy. A seed generation records the size of its built target, so a clone reserves cache budget from that figure instead of walking a multi-gigabyte seed first, and stage cleanup, generation selection, the clone and dep-info aging each get their own budget rather than sharing one two-second deadline. A budget fallback names the step that ran out and reports the effective value and its source in `cargo-target-setup.json` and the warning. A seed without a recorded size is accepted quietly (`size_recorded: false`), and an unreadable host config warns with its path and uses the compiled default. (#3620)
