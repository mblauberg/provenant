## Telemetry

### Value report

- `provenant telemetry value` now reports, for every account with quota readings and a configured plan, the API-equivalent cost per full window and its trend, the value multiple against the plan price in force, the share unused at reset with its observed-before-reset timestamp and sample count, and priced coverage. Credit accounts add a draw-down rate and days to exhaustion, or a named unavailable reason when the readings are missing. A reset with no reading before it, an unpriced bucket and an attribution gap are named rather than zero-filled; a failed quota-history read degrades those fields and names its cause. `price_source` and `window_source` state where the defaults came from. The accepted `telemetry cost --by` groupings are declared once in `CostBy` and checked against the runbook by a test. (#2225)
