## Behaviour changes

- Test doctrine prioritises extensible, flexible and reliable tests: public-boundary invariants and property-style checks replace rigid tests, exact count or string pins, and brittle example snapshots. Regression tests are minimal (at most one small check per real defect only where no invariant covers it), failing tests that pin implementation details are rewritten as invariants or deleted rather than bending production code, and deleting redundant tests is established as normal work across `skills/test-design`, `MAINTAINING.md`, and review lenses.
