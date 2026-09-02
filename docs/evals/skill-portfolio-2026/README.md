# Skill portfolio evaluation appendix

Evaluation evidence cut-off: 19 July 2026
Current-state note refreshed: 30 August 2026
Predecessor delivery run: `SKAUD-20260714`

This appendix is the durable evidence index for the 2026 portfolio refactor.
Detailed provider events and usage remain local run-owned evidence.

## Current catalogue holdout

| Dataset | Planned schedule | Result | Disposition |
|---|---:|---|---|
| [Current 25-owner holdout](routing-holdout.yaml) | 18 cases x 2 families x 3 trials | Not run | Catalogue holdout only; no current evaluation protocol has been frozen. |

The current holdout names the live consolidated owners `ui-ux-design`,
`skill-craft` and `autopilot`. It also tests the positive `setup-repo` trigger
against the negative boundary for ordinary mechanics in an already-configured
repository. Its 25-owner count is checked against `skills/*/SKILL.md` by the
live catalogue tests; it is not a rewrite of the frozen protocol.

The [frozen protocol](routing-protocol.json) remains evaluation v7's original
33-owner, 19 July artifact, including its source revision, identity and
daemon-era route. It is retained as planned-unexecuted historical evidence.

No semantic result file exists because no provider-backed model attempt started.
The semantic holdout remains provider-dependent and not run. A deliberate
current run must freeze a 25-owner protocol for the direct-provider batch path
and retain the actual route metadata. Do not reuse v7's retired
bootstrap-authority or provider-action assumptions.

The holdout therefore records catalogue coverage only. Any future trials must
record the actual provider/model-family route in a dispatch receipt and retain
every terminal action without retrying or relabelling failures.

## Current dispatch infrastructure fixture

The provider-free [dispatch fixture](../../../tests/fixtures/current-routing-eval/dispatch_fixture.py)
is exercised by the batch contract tests. It proves the current `batch_run.py`
and `dispatch_run.py` path retains a fixed bounded task set, fixture-emitted
route metadata for same- and mixed-family cases, an intentional partial
failure, and reducer inputs.
This is an infrastructure contract pass, not a semantic routing result: the
18-case holdout above remains unrun.

## Predecessor routing evidence

| Dataset | Schedule | Result | Disposition |
|---|---:|---|---|
| [14 July holdout](predecessor/routing-holdout-20260714.yaml) | 18 cases x 2 families x 3 trials | 108/108 exact primary-plus-bounded-companion routes | Historical predecessor only. |

The archived [predecessor protocol](predecessor/routing-protocol-20260714.json)
and [predecessor result](predecessor/routing-result-20260714.json) preserve the
original binding to commit `1ddfe24858b362decb1c507b87a466df26d205eb` at
`docs/evals/skill-portfolio-2026`. They evaluated retired owner names including
`frontend-review`, `skill-audit`, `skill-authoring` and `autonomous-lab`; they
do not establish current 25-owner routing behaviour.
Gemini 3.1 Pro High through `agy` and Grok 4.5 XHigh through `cursor-agent`
each completed three no-retry trials in that predecessor run.

## Retained non-passes

| Attempt | Result | Reason | Receipt SHA-256 |
|---|---|---|---|
| skill-audit v1 | fail | action-plus-audit rows gave primary ownership to `skill-audit` | `660422f2dd83f2dfc24435044e1ff0f4246c4373acb81c481e0353442b49eb15` |
| portfolio v3 | fail; 106/108 primary, 102/108 companions | the external-send artifact and audit composition were underspecified | `45530970b7417d703f0a64cad04dce179281d6ba2c5890690e272df969792338` |
| portfolio v4 | fail; 106/108 primary, 108/108 companions | two rows treated project-level Caveman wording as context, not invocation | `0f51749fa770797a135b3b6272f6fce6f1ef1010171e87a0f596e9adfffaf616` |

Earlier portfolio non-passes remain indexed in `summary.json`. They and the
14 July pass are historical evidence, not current routing results.

## Caveman quality evidence

The 16-case development comparison used baseline, one-line concise, rewritten
candidate and legacy arms, twice on each primary family. An opposite-family
blind judge scored 256 items after deterministic checks.

- Claude-family candidate cost was about USD 0.698 across two runs, versus
  0.633 for concise, 0.848 for baseline and 0.778 for legacy. This supports no
  universal savings claim.
- The pre-repair candidate had one Claude-family hard failure from raising an
  unverified source's evidence altitude; the skill was tightened. The
  OpenAI-family candidate had no hard failure and all five judge dimensions
  scored 4.0.
- A fresh three-case x three-trial x two-family altitude regression preserved
  attribution, non-causation and unconfirmed compromise in all 18 outputs.
  The lexical matcher marked 15/18 because three semantically correct phrases
  did not use its exact token; those are recorded as grader limitations rather
  than silently relabelled.
- Legacy was shorter but produced two Claude-family hard failures and lower
  OpenAI-family clarity. Brevity alone was not accepted as quality.

## Receipt hashes

| Evidence | SHA-256 |
|---|---|
| portable catalogue receipt | `5c2d6e0f1f545268ac40515059e97835bbb09f9ccb0ad19e6b606c0932ff4d5f` |
| writing regression receipt | `0ad0dfd979002a5418791c446e9909073fb75683670760f8119765fb965896c0` |
| selected host-overlay receipt | `31c192d6ddc69158bb2f829786ec737a28fd44912886524138414628a0625822` |
| Caveman arm receipt | `bc098ae63f98a2b433ee068beecd2821a9ad85b68861c9b3471517a27218714f` |
| Caveman blind-judge receipt | `e9f86375bebf4c1537ff214a62a3ceba494ede5c3f68e932129e8449e189fbec` |
| Caveman altitude-regression receipt | `4f9b9d4202a9cb8e2250fca1a7008adbdee578d774b1e3620eac256762d776e9` |
| predecessor routing evidence | original binding: commit `1ddfe24858b362decb1c507b87a466df26d205eb`, path `docs/evals/skill-portfolio-2026`; current copies: `predecessor/` |

## Interpretation limits

- These are synthetic routing and response-quality cases, not production task
  success rates.
- The portable catalogue excludes project-specific skills and most dynamic
  plugin overlays. The selected host probe contains 16 hand-selected entries.
- Model aliases, discovery behaviour, caching and pricing can change. Re-run
  after a material model, host or catalogue change.
- The current semantic holdout is unexecuted and provider-dependent. The
  provider-free infrastructure fixture establishes only the dispatch/batch
  contract, not behavioural routing quality; a deliberate direct-provider
  batch run is required for the semantic result.
- A completed routing regression establishes only its frozen synthetic cases.
  The enclosing delivery receipt and human retain acceptance and release
  authority.
