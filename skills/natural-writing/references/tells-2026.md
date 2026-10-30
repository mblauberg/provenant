# Tells in 2026 models (dated, expect churn)

Review date 30 October 2026. Tells rotate with each model release, so the
durable core lives in [mechanisms.md](mechanisms.md) and this file holds only
measurements that will age. Figures from one vendor corpus (marked S) need a
check before they are quoted.

## Still present in the newest models

- Contrastive framing (`more than a ...`, `rather than simply`) and
  `this matters` significance flagging. Graphite's corpus (S, relayed) puts
  both at tens of times the human rate even for recent Claude models.
- Mannered prose: metaphor and flourish in place of direct statement.
  Anthropic's Fable 5.1 prompting guide names it, and fixes it with a
  definition plus a worked contrast, not a phrase ban.
- Nominalisation and participle tails (Reinhart et al., PNAS 2025, arXiv
  2410.16107; 2024 models, so treat the tail figures as S).
- Vague positive evaluation, habitual triples, recap closers, and markdown
  headers and bullets in long documents (Wikipedia, Signs of AI writing).
- Sycophancy in advice (ELEPHANT, arXiv 2505.13995, ICLR 2026): validation
  and indirect language far above human rates, partly resistant to prompting.
- Over-explanation and low originality, the cues expert annotators relied on
  (Russell, Karpinska and Iyyer, ACL 2025, arXiv 2501.15654).

## Faded or contested

- `delve`-class vocabulary fell sharply in 2025 (Wikipedia). Kobak et al.
  (arXiv 2406.07016) measured the 2024 surge; the words turn over each release.
- Em-dash overuse is model-specific: high in GPT-4.1 and GPT-5, low in the
  newest Anthropic and Google models (S). Explicit bans failed in some models
  and tracked training habits ("The Last Fingerprint", arXiv 2603.27006).
- Heavy bullets and bold in short chat answers fell for Fable 5.1 (Anthropic).
  Remove old anti-formatting rules; ask when formatting helps the reader.

## How to steer

Say what to write, with the reason, plus two or three diverse before/after
pairs. Negative constraints can prime the banned token (arXiv 2601.08070, a
single-author preprint, weak evidence), which matches Anthropic's guidance to
state the target positively.

## Detector limits

Paraphrase defeats many detectors, error rates differ across writer groups,
and human judges are accurate only in clusters. Never accuse a writer on a
score, and never degrade clear prose to evade one.
