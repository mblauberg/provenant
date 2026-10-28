# Model routing
Owner: provenant-routing
Anchor: tools/front-door/crates/routing/src

Run `provenant route list` to inspect the catalogue, configured pool weights
and pacing policy; pacing and readiness apply at dispatch. Use
`provenant --json route list` for records, `--adapter NAME` to narrow it, or
`--pool NAME` for one pool. Resolve a pool with
`provenant route resolve --route NAME`, or select an exact model with
`--model ADAPTER/ID` and optional `--effort LEVEL`.
`provenant route resolve NAME` also accepts a route name or an unambiguous
model name. Unknown or ambiguous names report suggestions and examples.

The resolver draws among ready positive-weight members using weight multiplied
by the provider pacing factor. `pace` uses the selected account's latest fresh
quota snapshot and takes the lowest factor across windows with a future reset.
Each factor divides remaining quota headroom by remaining window time and is
clamped to 0.25–2.0. A five-hour window at 85% used or a seven-day window at
90% used makes the account unavailable with `pacing_reserve`. Override the
default 15% and 10% reserves per adapter in `config.toml`, for example:

```toml
[domains.routing.pacing_reserve.claude]
five_hour_percent = 15
seven_day_percent = 10
```

Reserve percentages range from 0 to 100. Other window durations affect the
factor but have no reserve. Expired resets do not affect pacing unless an
active dashboard calibration still binds that window. Calibrated windows with
no current reset use a full remaining window for conservative pacing; see
[Go calibration](configuration.md#opencode-go-quota-calibration). Go pool
members require fresh quota with all three allowance windows and complete
coverage or an active floor for each window. Other missing quota data uses
1.0. `spend` and `max` use 1.0; admission handles
short rate-limit cooldowns, and a paid route remains eligible while credits
remain. If no weighted member is ready, the first ready zero-weight member is
selected; otherwise a recoverable unavailable member is preferred, and
disabled members are excluded. If only disabled members remain, resolution
returns their exclusions. The receipt records candidate readiness and
exclusions. Operation ID and task index seed selection so retries repeat the
same choice. Only OpenCode `*-free` entries are marked free and they require
`--allow-free` (default false). Agy Gemini Flash is a paid subscription model.
Omit `--effort` on OpenCode free routes: their recorded model controls do not
support effort, so an explicit value is refused before launch.
`--confidential` excludes training or unclassified members and fallbacks.
There is no classifier or route-synonym inference. Effort is set only when the
catalogue contains an exact recorded control; unsupported effort is refused.

The bundled Rust catalogue (`tools/front-door/crates/routing/assets/catalogue.toml`)
owns default pool membership, weights, pacing and effort controls. Override a
member with `provenant route set POOL MODEL [--weight N] [--effort E]
[--efforts a,b,c]`, remove one with `provenant route remove POOL MODEL`, or
restore a bundled pool with `provenant route reset POOL`. Set provider pacing
with `provenant route pacing PROVIDER pace|spend|max`. These edits are stored
under `[domains.routing]` in the selected state's `config.toml`. Catalogue
presence does not establish adapter availability; readiness is decided at
dispatch, so inspect the resolved route recorded for the attempt.

Explicit fallbacks use `--fallback ADAPTER/ID,ADAPTER/ID`.

Dispatch uses the same route flags. Copy the resolved `adapter/model@effort`
from the attempt record; never infer a model from a tier or a provider name.
Quota observations use `provenant quota` and `provenant quota tick`.

Agy read-only lanes can run system commands, including Git, within the outer
read-only confinement profile. Headless tool permissions are auto-approved;
the profile still restricts writes to attempt scratch and private state. If
Agy reports a denied action, the terminal reason names it, for example
`provider denied: RunCommand`.

The routing, providers and quota crates define the records and eligibility
policy. Run `provenant help route` for flags.
