## Fixes

### Lanes and settlement

- A dead attempt whose stored record no longer decodes no longer fences its branch forever. `reconcile` now classifies the decode failure as an unreadable record, and once writer exclusion and native evidence prove the owner gone it preserves the raw bytes under the attempt's `quarantine/` and releases the writer lease; `release attempt:ID` offers the same safety valve. Both emit one host-side typed warning (`UNREADABLE_RECORD_RELEASED`) whose subject names the record path, schema version and error. An unobservable or live holder keeps the fence, admission refusals stay fixed text, and neither collection nor release deletes or rewrites the stored record. (#3829)
