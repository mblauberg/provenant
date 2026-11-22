# Engineering stance

The owner's standing design preferences. They are concerns to weigh against the
case in hand, not checklists: name the alternatives and say which one you chose
and why.

## Root cause and design

- Fix the cause, not the symptom. Sweep for the class of defect and fix what
  let it through. Repeated flaws warrant a design review.
- Long-term structure over patches. A larger redesign is welcome and lands as
  proportionate changes. If a small outcome needs a lot of new code, look for a
  simpler structure.

## Build for change

- **Extensible.** Each mechanism is one declared core (registry, declaration,
  table or setting) and a new case is one entry. Nothing is kept by hand in
  parallel lists. Before adding a policy or rule, check whether an existing
  declaration already encodes it; if it is wrong, fix that declaration rather
  than layering a second.
- **Configurable.** Behaviour that varies is data or a config key with its
  source shown.
- **Modular.** One owning module per concern with a small interface. Modules
  compose through ports and declarations, so one can be replaced without edits
  elsewhere.
- **General where it pays.** Building a core ahead of a second use is fine;
  avoid only indirection that adds cost without making change easier.

## Reliability and strictness

- Failures are explicit typed states, never silent fallbacks. Catch mistakes
  before runtime with types, lints and generators.
- Checks default to allow or warn and have one deciding point. Add no new
  refusal outside the three hard boundaries (credentials, other lanes' work,
  authority). Use coordination where correctness needs it; prefer structural
  speed improvements.
