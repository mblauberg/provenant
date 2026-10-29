# OpenCode Go model admission, 28 October 2026

Current policy limits the OpenCode catalogue to the free Zen and paid Go variants of DeepSeek V4.1 Flash and GLM-5.3 Flash. Other models discussed below are historical research and are no longer catalogued.

The [routing catalogue](../../tools/front-door/crates/routing/assets/catalogue.toml)
owns current pool membership. This note records the evidence for the Batch O
admission decision; provider benchmark claims are priors, not local trial results.
All sources below were accessed on 28 October 2026.

## Monthly allowance policy

The [Go pricing table](https://opencode.ai/v2/docs/console/go/), accessed on
28 October 2026, supplies the catalogue's monthly allowance values for the
$10 plan. The routing floor defaults to $60; models below it or without a
recorded allowance are refused while it is enabled, including exact selections.
The table lists Beam Free and Iceman as free for a limited time, with no
published end date, and Claude Haiku 5.5 at $15. OpenCode states that Iceman
uses prompts and completions to improve the model. Zen models use a separate
account and are not subject to this floor.
The IDs for the other documented models at or above $60 were checked against
the [models.dev provider catalogue](https://models.dev/api.json), accessed on
28 October 2026. They are no longer catalogued.

## Excluded models

MiMo V2.6 Pro and GLM-5.3 are not catalogued. Each draws the Go allowance at
roughly four times the rate of the Flash models (a $15 monthly limit on the $10
plan, against $60 to $80), and the Flash models are strong enough for both pools.

MiMo V2.6 Flash is no longer catalogued. Xiaomi reports
Terminal-Bench 2.1 87.6 for it in its
[model card](https://huggingface.co/XiaomiMiMo/MiMo-V2.6-Flash-RL), but no
independent score places it ahead of DeepSeek V4.1 Flash or GLM-5.3 Flash, and it
costs the same allowance as GLM-5.3 Flash.

## Flash fallbacks

The free Zen variants of DeepSeek V4.1 Flash and GLM-5.3 Flash already sit in
the junior pool at weight 50 each, but free allowances run out. Weighted
selection favours them without guaranteeing they are exhausted first. Their paid
Go endpoints are the cheapest fallbacks: the [Go documentation](https://opencode.ai/v2/docs/console/go/)
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

## Catalogue refresh, 29 October 2026

The [Go pricing table](https://opencode.ai/v2/docs/console/go/), accessed on
29 October 2026, lists newer releases in two families. The catalogue previously carried
MiniMax M3.1 Flash ($20 monthly limit on the $10 plan) in place of MiniMax M3,
and Qwen3.8 Flash ($30) in place of Qwen3.7 Plus. Qwen3.8 Flash is preferred to
Qwen3.8 Max ($15) because it draws the allowance at half the rate. Both replacements have since been removed from the catalogue.

Kimi K2.7 Code, MiMo V2.5 and Muse Spark 1.2 Contributor were removed as
superseded. Claude Haiku 5.5 was removed because the Claude adapter already
serves it from the Claude subscription, at a fraction of the Go allowance cost.

## Provider controls

The [OpenCode Go documentation](https://opencode.ai/docs/go/) lists these
hosted models and describes Go's privacy settings. The catalogue records
`trains_on_prompts = false` for these paid endpoints as required by the admission
scope. No effort controls or underlying model-family identity are inferred from
the research. Paid OpenCode pacing remains `pace`; free-model pacing remains
`max`. Go pacing uses the local ledger and any active calibration floors, as
described by the [routing owner](../specs/routing.md).
