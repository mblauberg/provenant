# OpenCode Go model admission

## Flash fallbacks

Free model allowances can be exhausted, so paid Flash models on the Go plan provide fallback capacity. Their cost against the monthly allowance (`monthly_allowance_usd`) informed admission; benchmark claims are priors and do not establish local quality ([ADR 0033](../adr/0033-weighted-route-pools-are-the-routing-unit.md)). The [routing catalogue](../../tools/front-door/crates/routing/assets/catalogue.toml) owns current membership and weights, [`telemetry-prices.toml`](../../config/telemetry-prices.toml) owns rates, and the [routing spec](../specs/routing.md) owns admission policy.

Sources: [OpenCode Go](https://opencode.ai/docs/go/), [DeepSeek Flash](https://artificialanalysis.ai/models/releases/deepseek-v4-1-flash) and [GLM Flash](https://artificialanalysis.ai/models/glm-5-3-flash/).

## Go limits

[docs] Go limits are 20% of the monthly allowance per 5 hours, 50% per week and the monthly allowance. `frames` in [`quota_live/opencode.rs`](../../tools/front-door/crates/providers/src/quota_live/opencode.rs) encodes these as window shares of the configured cap and fills them from the host's own database, a lower bound that excludes other devices. Credential and readiness behaviour is in the [interface study](opencode-interface.md#credentials-and-readiness).
