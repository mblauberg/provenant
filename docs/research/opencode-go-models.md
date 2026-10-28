# OpenCode Go model admission, 28 October 2026

The [routing catalogue](../../tools/front-door/crates/routing/assets/catalogue.toml)
owns current pool membership. This note records the evidence for the Batch O
admission decision; provider benchmark claims are priors, not local trial results.
All sources below were accessed on 28 October 2026.

## MiMo V2.6

The [MiMo-V2.6-Flash-RL model card](https://huggingface.co/XiaomiMiMo/MiMo-V2.6-Flash-RL)
describes the series' reinforcement learning across coding and agent tasks and
positions Flash as its efficiency-focused checkpoint. This supports a provisional
trial of `opencode/opencode-go/mimo-v2.6-flash` in both junior and senior pools,
with weight 1 in each. It does not establish that the hosted endpoint is identical
to that checkpoint or that it has passed Provenant's own task trials.

`opencode/opencode-go/mimo-v2.6-pro` is catalogued for exact-model trials only.
Senior pool admission awaits a trial.

## GLM 5.3

The [GLM-5.3 announcement](https://z.ai/blog/glm-5.3) reports improvements on coding
and long-running tasks. Its page renders the article from JavaScript; the article
asset was inspected where the plain HTML contained only the application shell.
`opencode/opencode-go/glm-5.3` is catalogued for exact-model trials only. Senior
pool admission awaits a trial.

## Provider controls

The [OpenCode Go documentation](https://opencode.ai/docs/go/) lists all three
hosted models and describes Go's privacy settings. The catalogue records
`trains_on_prompts = false` for these paid endpoints as required by the admission
scope. No effort controls or underlying model-family identity are inferred from
the research. Paid OpenCode pacing remains `pace`; free-model pacing remains
`max`. Pool dispatch additionally requires trustworthy Go quota, as specified by
the [routing owner](../specs/routing.md).
