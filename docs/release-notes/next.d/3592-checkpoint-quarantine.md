## Fixes

### Pipelines and graphs

- A graph checkpoint whose digest does not verify is no longer retried on every tick. The first host tick read reports it once with its typed reason (`bad_digest`, `decode`, and so on), records the failing bytes in a quarantine marker, and later ticks over the same bytes skip it instead of rebuilding the same warning on every pass. A checkpoint whose bytes change is reported again, restoring valid bytes clears the quarantine, and `provenant stop -- NAME` retires the record. Status and strict reads still show the corrupt graph with its ID, and other graphs keep indexing. (#3592)
