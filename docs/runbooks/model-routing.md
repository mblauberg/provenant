# Model routing
Owner: provenant-routing
Anchor: tools/front-door/crates/routing/src

Run `provenant route list` to inspect the catalogue, configured pool weights
and pacing policy; pacing and readiness apply at dispatch. Use
`provenant --json route list` for records, `--adapter NAME` to narrow it, or
`--pool NAME` for one pool. Resolve a pool with
`provenant route resolve --route NAME`, or select an exact model with
`--model ADAPTER/ID` and optional `--effort LEVEL`.

The bundled pools use weighted ready members and preserve listed order for
zero-weight fallback members:

| Pool | Members and weights |
|---|---|
| `utility` | Claude Haiku@low w4, Codex Luna@low w2, DeepSeek Flash Free w1, GLM Flash Free w1 |
| `junior` | Codex Luna@high w4, Claude Haiku@high w4, Agy Gemini@medium w2, Agy Sonnet 4.6 w1, two OpenCode free models w1 each |
| `senior` | Codex Sol@high w4, Claude Sonnet 5.5 w4, Agy Gemini@high w2, Agy Opus 4.6 Thinking w1, Cursor Grok 4.7 w1 |
| `principal` | Claude Opus 5.5 w1, Codex Astra w0 fallback |
| `writing` | Agy Gemini w4, Claude Haiku w4, Agy Sonnet 4.6 w2, Claude Sonnet 5.5 w1, Codex Luna@high w1 |
| `design` | Claude Sonnet 5.5 w4, Agy Gemini@high w2, Claude Opus 5.5 w1 |

The resolver draws among ready positive-weight members using weight multiplied
by the provider pacing factor. `pace` uses the best fresh quota snapshot for
the provider and its shortest window with a reset; it divides remaining
quota headroom by remaining window time and clamps the factor to 0.25–2.0.
Missing quota data uses 1.0. `spend` and `max` use 1.0; admission handles
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
Quota observations use `provenant quota show` and `provenant quota tick`.

The routing, providers and quota crates define the records and eligibility
policy. Run `provenant help route` for flags.
