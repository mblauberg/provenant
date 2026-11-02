# Route readiness

Quota owns one readiness record per host, adapter, account and canonical model
or verified pool. It records a schema version, state (ready, unavailable or
unknown), typed reason, observation and expiry times, optional reset time and a
bounded non-secret evidence reference. Providers supply facts; quota publishes
verdicts. Selection gates for access, confinement, effort and pacing remain
contextual. Reasons include not probed, stale, probe timeout, command failure,
missing executable, signed out, account mismatch, exhausted, provider refusal,
uncalibrated partial coverage, invalid evidence, unreachable peer and
unsupported peer version. Existing exclusion reason wire names stay stable.

Missing records, future observations, scope mismatches and records at or beyond
their expiry read as unknown. A reset does not by itself establish readiness.
A partial, uncalibrated usage window cannot establish headroom; a lower bound at
the limit still establishes exhaustion. Account refusals cover all routes for
that account; pool refusals cover only the verified pool. Refusals publish as
soon as observed and retain the existing clearing and isolation rules.

Persist readiness separately at `quota/<account-key>/<adapter>.readiness.json`,
under the account file's lock and atomic replacement protocol. An absent file
reads as unknown and schedules a probe. Reuse the quota probe interval. A changed
agy fingerprint makes the old account unavailable and registers the new identity.

Routing, route commands, dispatch, host diagnostics, peer responses and quota
displays consume this record. Route previews do not perform host I/O unless
explicitly asked to probe. Receipt wire names remain compatible.
