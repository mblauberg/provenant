## Behaviour changes

### CLI and install

- Oversized `console`, `disk report` and other command results no longer fail with `RESULT_TOO_LARGE`: they warn with the cap's source and the omitted byte count, then show a bounded view or page the result to an artefact (#3551)
- `provenant wait` inside a lane no longer refuses when no targets are named; it warns and waits on the lane's own children and relay-discovered targets (#3551)
- Unreadable or malformed wait configuration now warns with the configuration source and uses the compiled defaults instead of aborting the wait (#3551)
- A disabled verify port warns with the port's source and returns a clear skipped result instead of an `unavailable` refusal, and an unreadable tracker port warns and falls back to compiled defaults (#3551)
