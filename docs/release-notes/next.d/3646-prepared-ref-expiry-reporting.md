## Fixes

### Git and safety

- A prepared Git reference transaction aborted because its callback ran past the deadline now sets `timed_out` on the execution outcome. Previously Git could write `abort: ok` and exit before the stop-signal path ran, leaving the outcome to report neither expiry nor cancellation and making a deadline indistinguishable from a refusal; the reference was still kept. (#3646)
