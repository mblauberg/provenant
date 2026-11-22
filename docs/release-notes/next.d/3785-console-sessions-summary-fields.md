## Changes

### CLI and install

`console sessions` publishes its one-line display text as `summary_line` and the
source as `summary_source` on each row; the top-level string `summary` is gone and
the structured summary stays at `record.summary`. The sessions data schema version
stays 2: this is an additive rename on an evolving envelope (#3785).
