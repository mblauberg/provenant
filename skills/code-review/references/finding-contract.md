# Finding contract

## Finding threshold

Report a finding only when all are present:

1. an exact file/line, symbol, command, artefact, or reproducible flow;
2. the failure mechanism or structural regression;
3. a concrete impact;
4. an actionable remedy or validation route;
5. confidence high enough to survive an adversarial reread.

A claim that something does not exist, is missing or is unused cites the
search, file:line or doc that shows it. Without that evidence it is
`needs-evidence`, and the chair reruns the check before it counts as a blocker.

Label provenance:

- `introduced`: caused by the reviewed change;
- `revealed`: pre-existing condition made relevant or dangerous by the change;
- `pre-existing`: unrelated debt, normally omitted.

## Severity

- `P0`: immediate catastrophic loss, compromise, or unusable system.
- `P1`: likely serious correctness, security, data-integrity, or compatibility
  failure; blocks acceptance.
- `P2`: material defect, regression, or structural change that raises present
  failure/maintenance risk; normally blocks until resolved or explicitly
  accepted.
- `P3`: bounded non-blocking improvement with demonstrated value.

Style preferences, arbitrary size limits and unproven redesigns are not
findings. State uncertainty and the falsification step instead of inflating
severity.

## Disposition

At adjudication, the chair records each finding as `confirmed`,
`false_positive`, `reviewer_bias` or `duplicate` with
the Rust workflow owner; duplicate findings also name
`--duplicate-of`.

## Output

Order findings by severity, then dependency impact. Use:

```text
[P1][introduced] path/file.ts:42 — Short title
Mechanism: concrete failure sequence or design regression.
Impact: who or what fails.
Fix: smallest safe remedy and validation route.
Evidence: test, command, caller, schema, or source anchor.
```

When the user asks for compact findings, keep the same fields on one line:

```text
[P1][introduced] path/file.ts:42 — problem; mechanism/impact; fix; evidence.
```

Compression changes presentation, never the proof threshold or coverage.

After findings, add only if useful:

- a compact review-surface receipt: inspected diff/files/callers/contracts and
  excluded or unavailable surfaces;
- unresolved questions that prevent a verdict;
- verification lanes that did not run;
- non-blocking structural opportunities with their proof burden met.

If no genuine finding survives verification, return `clean`. Do not manufacture
nits to demonstrate effort.
