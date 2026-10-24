# Model routing

Run `provenant route list` to inspect the catalogue, ordered pool members and
cached availability. Use `provenant --json route list` for records, or
`--adapter NAME` to narrow it. Resolve a pool with
`provenant route resolve --route NAME`, or select an exact model with
`--model ADAPTER/ID` and optional `--effort LEVEL`.

The bundled pools are ordered readiness fallbacks:

| Pool | Members, in preference order |
|---|---|
| `utility` | Claude Haiku, Codex Luna at recorded `low`, OpenCode DeepSeek Flash Free, OpenCode GLM Flash Free |
| `junior` | Codex Luna, Agy Gemini 3.8 Flash, Claude Haiku, the two OpenCode free models |
| `senior` | Codex GPT-6.1 Sol, Claude Sonnet 5.5, Agy Gemini 3.8 Flash |
| `principal` | Claude Opus 5.5, Codex GPT-6 Astra |
| `writing` | Codex Luna, Agy Gemini 3.8 Flash, Claude Sonnet 5.5 |
| `design` | Claude Sonnet 5.5, Agy Gemini 3.8 Flash, Claude Opus 5.5 |

The resolver selects the first ready member; if none is ready, it reports the
first eligible member with its readiness blocker. It does not rotate or weight
members. Only the OpenCode `*-free` entries are marked free and they require
`--allow-free` (default false). Agy Gemini Flash is a paid subscription model.
`--confidential` excludes training or unclassified members and fallbacks.
There is no classifier or route-synonym inference. Effort is set only when the
catalogue contains an exact recorded control; unsupported effort is refused.

The bundled Rust catalogue (`tools/front-door/crates/routing/assets/catalogue.toml`)
is authoritative for pool membership, order and effort controls. Catalogue
presence does not establish adapter availability; check cached readiness and
the resolved route recorded for the attempt.

Explicit fallbacks use `--fallback ADAPTER/ID,ADAPTER/ID`.

Dispatch uses the same route flags. Copy the resolved `adapter/model@effort`
from the attempt record; never infer a model from a tier or a provider name.
Quota observations use `provenant quota show` and `provenant quota tick`.

The Python catalogue, account reset commands, cooldown files and routing
wrappers are retired. The Rust routing/providers/quota owners define the
current records and eligibility policy. Run `provenant help route` for flags.
