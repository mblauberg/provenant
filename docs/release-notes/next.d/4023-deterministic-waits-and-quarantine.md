## Fixes

### CI and tests

- A red `main` caused by a known flake no longer opens a duplicate `ci:red-main` issue. `main-health` reads the JUnit results of the failed run, and a re-run job executes the exact Linux tests that failed once; a failure that passes on re-run is registered as one `ci:flaky` issue with an owner and a 30-day expiry, joins the quarantine filter and keeps running advisory. TOML and issue-backed entries share one validated schema and a cap of ten, so a full quarantine leaves new failures actionable. Suppression and registration are recorded in [CI](../../specs/ci.md#quarantine-is-bounded-and-evidence-backed). (#4023)
- Test fixtures now wait on an event or pass their deadline to `provenant-test-support::wait_until` and `wait_step`, which own the polling cadence; the supervisor custody group names no fixed sleep. `workspace_policy` warns on fixed waits in test scopes and refuses them under `PROVENANT_STRICT_TEST_WAITS=1`. (#4023)
