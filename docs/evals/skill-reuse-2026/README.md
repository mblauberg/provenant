# Skill-reuse routing evaluation

Status: evaluator pass at the declared current-candidate threshold and paired
non-regression check; the cross-primary-family promotion gate is unmet.

The frozen dataset contains all ten canonical routing cases changed by the
skill-reuse repair. Every retained packet excludes expected routes. Three fresh,
no-retry candidate subscription invocations used GPT-5.6 Luna, Terra and Sol;
Luna ran at the CLI default (`none`) and the other two at low reasoning effort.
The candidate scored 29/30 exact primary-plus-companion routes: Luna and Sol
were exact, while Terra added an unnecessary `typescript-clean-code` companion
to q058. That non-pass is retained rather than retried or relabelled.

One no-retry Luna comparison used only the candidate's skill names, without
descriptions; one used the exact `origin/main` package catalogue. Both scored
10/10. The paired Luna candidate and previous-package arms therefore show no
regression on this holdout. The comparison does not establish statistical
superiority: the without-skill result also scored 10/10, and each comparison
arm has only one trial.

The required across-current-primary-families comparison is incomplete because
Claude subscription quota was unavailable. The retained OpenAI-only arms are
useful evaluator evidence, but they do not clear the MAINTAINING promotion
gate. Gemini or Grok review would be additional diversity, not a substitute for
the missing equal-primary Claude family.

`receipt.json` binds the dataset, complete candidate catalogue, classifier,
packet and raw JSON outputs to candidate commit
`1bb67fd77c7aaabc6e8917c2c4f6a404c1e090a7` and tree
`07a5b1df35e9b7489a2398b1928d445f49832c9c`. The receipt was validated from
the repository root at the time of the evaluation. The evaluator has since
been retired; this directory is retained as historical evidence and is not a
current gate.

`attempts.json` retains two response-schema rejections that occurred before
inference and two semantic non-passes. Raw CLI errors for the pre-inference
rejections were not retained, and the manifest says so explicitly. The semantic
failure packets and outputs remain under `attempts/`; the validator binds their
candidate trees and input digests and recomputes 23/33 and 21/24. No failed row
was relabelled or copied into the passing run.

These are direct Codex CLI subscription invocations. The receipt records that
adapter honestly and does not claim Fabric provider-action certification,
cross-provider independence, statistical superiority, production task success
or human acceptance. The receipt therefore declares
`evaluation-pass-promotion-gate-unmet`, not promotion readiness.
