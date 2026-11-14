# Engineering stance

The owner's standing design preferences. Apply them when you write, review,
scope or test. They are concerns to weigh against the case in hand, not
checklists: name the alternatives and say which one you chose and why.

## Root cause and design

- Fix the cause, not the symptom. Sweep for the class of defect and fix what
  let it through. A flaw that returns after a second fix means the design is
  wrong: change the design.
- Long-term structure over patches. A larger redesign is welcome and lands as
  one complete change.
- If a small outcome needs a lot of new code, stop and simplify.

## Build for change

- **Extensible.** Each mechanism is one declared core (registry, declaration,
  table or setting) and a new case is one entry. Nothing is kept by hand in
  parallel lists. Before adding a policy or rule, check whether an existing
  declaration already encodes it; if it is wrong, fix that declaration rather
  than layering a second.
- **Configurable.** Behaviour that varies is data or a config key with its
  source shown, never a branch or special case for one host, project or
  provider.
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
  refusal, lock, floor, queue or wait. Hard holds belong only at real
  boundaries: credentials, Git isolation (one writer per worktree), data
  integrity and authority (who may approve or merge). When busy, skip and
  retry. Speed comes from structure (caching, derivation), not locks.

## No compatibility code

No aliases, older-shape readers, migrations, fallbacks or deprecated keys. Move
every caller and delete the old shape. Follow best practice, not the current
shape of the codebase.

## Tests

Tests pin invariants derived from declarations (parameterised or property
tests), with modular, deterministic fixtures. Do not assert exact messages,
counts, order or snapshots unless that is the invariant. Question a failing
test first: redesign or delete it if it is stale or rigid. See `test-design`.
