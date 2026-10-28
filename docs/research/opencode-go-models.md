# OpenCode Go model admission, 28 October 2026

The [routing catalogue](../../tools/front-door/crates/routing/assets/catalogue.toml)
owns current pool membership. This note records the evidence for the Batch O
admission decision; provider benchmark claims are priors, not local trial results.
All sources below were accessed on 28 October 2026.

## Excluded models

MiMo V2.6 Pro and GLM-5.3 are not catalogued. Each draws the Go allowance at
roughly four times the rate of the Flash models (a $15 monthly limit on the $10
plan, against $60 to $80), and the Flash models are strong enough for both pools.

MiMo V2.6 Flash stays catalogued for exact selection only. Xiaomi reports
Terminal-Bench 2.1 87.6 for it in its
[model card](https://huggingface.co/XiaomiMiMo/MiMo-V2.6-Flash-RL), but no
independent score places it ahead of DeepSeek V4.1 Flash or GLM-5.3 Flash, and it
costs the same allowance as GLM-5.3 Flash.

## Flash fallbacks

The free Zen variants of DeepSeek V4.1 Flash and GLM-5.3 Flash already sit in
the junior pool, but free allowances run out. Their paid Go endpoints are the
cheapest fallbacks: the [Go documentation](https://opencode.ai/v2/docs/console/go/)
gives DeepSeek V4.1 Flash an $80 monthly limit on the $10 plan (8x) and GLM-5.3
Flash $60 (6x), the same as MiMo V2.6 Flash. Artificial Analysis rates
[DeepSeek V4.1 Flash](https://artificialanalysis.ai/models/releases/deepseek-v4-1-flash)
at 45 and [GLM-5.3 Flash](https://artificialanalysis.ai/models/glm-5-3-flash/) at
42 on its intelligence index. DeepSeek's 1017 revision (17 October 2026) reports
no Terminal-Bench 2.1 score, so the older Flash revision's figure is not
attributed to it, and OpenCode does not publish which revision it serves. DeepSeek V4.1
Flash joins the junior pool at weight 2 and GLM-5.3 Flash at weight 1; both join
the senior pool at weight 1. These are provisional weights pending local trial
evidence.

## Provider controls

The [OpenCode Go documentation](https://opencode.ai/docs/go/) lists these
hosted models and describes Go's privacy settings. The catalogue records
`trains_on_prompts = false` for these paid endpoints as required by the admission
scope. No effort controls or underlying model-family identity are inferred from
the research. Paid OpenCode pacing remains `pace`; free-model pacing remains
`max`. Pool dispatch additionally requires trustworthy Go quota, as specified by
the [routing owner](../specs/routing.md).
