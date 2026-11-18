# Engineering stance

The owner's standing design preferences. Apply them when you write, review,
scope or test. They are concerns to weigh against the case in hand, not
checklists: name the alternatives and say which one you chose and why.

## Root cause and design

- Fix the cause, not the symptom. Sweep for the class of defect and fix what
  let it through. Repeated flaws warrant a design review.
- Long-term structure over patches. A larger redesign is welcome and lands as
  proportionate changes.
- If a small outcome needs a lot of new code, look for a simpler structure.

## Build for change

- **Extensible.** Each mechanism is one declared core (registry, declaration,
  table or setting) and a new case is one entry. Nothing is kept by hand in
  parallel lists. Before adding a policy or rule, check whether an existing
  declaration already encodes it; if it is wrong, fix that declaration rather
  than layering a second.
- **Configurable.** Behaviour that varies is data or a config key with its
  source shown where configuration makes variation easier to manage.
- **Modular.** One owning module per concern with a small interface. Modules
  compose through ports and declarations, so one can be replaced without edits
  elsewhere.
- **General where it pays.** Designing for foreseeable cases is welcome, and a
  core built ahead of any second use is fine. Avoid only indirection that adds
  cost without making change easier.

## Reliability and strictness

- Failures are explicit typed states, never silent fallbacks. Catch mistakes
  before runtime with types, lints and generators.
- Checks default to allow or warn and have one deciding point. Add no new
  refusal without a real boundary: credentials, Git isolation, data integrity, authority or platform safety. Use coordination where needed for
  correctness; prefer structural speed improvements.

## No compatibility code

Move callers to the new shape without compatibility code. Follow the owning
repository’s format policy for persisted data.

## Tests

See the `test-design` skill for test invariants and failure triage.
