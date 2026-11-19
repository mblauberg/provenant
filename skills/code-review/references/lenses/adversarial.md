# Lens: adversarial

Shared guidance: [review-lenses.md](../review-lenses.md).

Assume the change is wrong and try to prove it. Pick the three most damaging
ways it could fail in production or be abused, then trace each through the code
and tests until you either confirm a defect with evidence or can state why it
cannot happen. Report only findings you could demonstrate, and list the
hypotheses you rejected.

Abuse means breaching a real boundary: credentials, other lanes' work or
authority. Shared caches, targets, co-writers and relaxed checks are intended,
not findings.
