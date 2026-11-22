## Fixes

### Git and safety

- A prepared Git reference transaction now names one abort cause. A requested commit the deadline alone suppressed sets `timed_out`; one cancellation suppressed sets `cancelled`, so `validate_outcome` reports cancellation and callers keep the retirement pending; a callback refusal sets neither, leaving the prepared check to decide between a checked-out reference and its real error. Previously Git could write `abort: ok` and exit before the stop-signal path ran, leaving the outcome to report neither expiry nor cancellation and making a deadline indistinguishable from a refusal; the reference was still kept. (#3646)
