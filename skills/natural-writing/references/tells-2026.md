# Tells in 2026 models (dated, expect churn)

Review date 30 October 2026. Tells rotate with each model release, so the
durable core lives in [mechanisms.md](mechanisms.md) and this file holds only
measurements that will age. Figures from one vendor corpus (marked S) need a
check before they are quoted.

## Still present in the newest models

| Tell | Mechanism | Evidence |
| --- | --- | --- |
| Contrastive framing; `this matters` significance flagging | 1, 2 | Graphite's corpus (S, relayed): both at tens of times the human rate, even for recent Claude models |
| Mannered prose: metaphor and flourish in place of direct statement | 3 | Named in Anthropic's Fable 5.1 prompting guide, which fixes it with a definition plus a worked contrast, not a phrase ban |
| Nominalisation; participle tails | 5, 6 | Reinhart et al., PNAS 2025, arXiv 2410.16107; 2024 models, so treat the tail figures as S |
| Vague positive evaluation; habitual triples; recap closers; markdown headers and bullets in long documents | 2, 7, 8, 10 | Wikipedia, Signs of AI writing |
| Sycophancy in advice | 11 | ELEPHANT, arXiv 2505.13995, ICLR 2026: validation and indirect language far above human rates, partly resistant to prompting |
| Over-explanation; low originality | 4, and [condense-pass.md](condense-pass.md) | The cues expert annotators relied on (Russell, Karpinska and Iyyer, ACL 2025, arXiv 2501.15654) |

## Faded or contested

| Tell | Status | Source |
| --- | --- | --- |
| `delve`-class vocabulary | Fell sharply in 2025; the words turn over each release | Wikipedia; Kobak et al., arXiv 2406.07016, measured the 2024 surge |
| Em-dash overuse | Model-specific: high in GPT-4.1 and GPT-5, low in the newest Anthropic and Google models (S). Explicit bans failed in some models and tracked training habits | "The Last Fingerprint", arXiv 2603.27006 |
| Heavy bullets and bold in short chat answers | Fell for Fable 5.1. Remove old anti-formatting rules; ask when formatting helps the reader | Anthropic |

## How to steer

Say what to write and why, then give two or three diverse before/after pairs.
Negative constraints can prime the banned token (arXiv 2601.08070, a
single-author preprint, so weak evidence), which matches Anthropic's guidance
to state the target positively.

## Detector limits

Paraphrase defeats many detectors, error rates differ across writer groups,
and human judges are accurate only in clusters. Never accuse a writer on a
score, and never degrade clear prose to evade one.
