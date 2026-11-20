# OpenCode Go model admission

## Flash fallbacks

Free model allowances can be exhausted, so paid Flash endpoints provide fallback capacity. Their relative allowance cost informed admission; benchmark claims are priors and do not establish local quality. The [routing catalogue](../../tools/front-door/crates/routing/assets/catalogue.toml) owns current membership and weights; the [routing spec](../specs/routing.md) owns admission policy.

Sources: [OpenCode Go](https://opencode.ai/docs/go/), [DeepSeek Flash](https://artificialanalysis.ai/models/releases/deepseek-v4-1-flash) and [GLM Flash](https://artificialanalysis.ai/models/glm-5-3-flash/).

## Observed 2026-11-20

Findings from [the interface study](opencode-interface.md); each is dated and observed first-hand unless marked docs.

- Go pricing (docs): Go is $10 per month, Go Plus $40, Go Teams $20 per member. Limits are 20% of the monthly allowance per 5 hours, 50% per week (resets Monday 00:00 UTC) and the monthly allowance. At the limit, free models still work or Extra Usage falls back to Console credit. The 2.2.4 changelog raised Go limits for DeepSeek V4.1 Flash.
- Catalogue (observed): `opencode-go/deepseek-v4.1-flash` lists at 0.15 input and 0.60 output per million tokens with variants `low`, `high`, `max`. The Zen free route `opencode/deepseek-v4.1-flash-free` has a 200,000 context and 32,000 output cap. `opencode/hy4-flash-free` has variants `none` and `high`.
- Readiness: `opencode models` lists a model when a credential for its provider exists, not when the credential works. A stale `auth.json` Go key lists all 34 Go models yet answers `401 api_key_expired`. The working credential is the Console login stored in `opencode.db`.
