## Fixes

### Review and repair

- Blocking findings now name the invariant they break in their top-level `class`. A finding without one is warned first at settlement; a blocking finding without one is refused. A fix round returns, per invariant, the `sweep` it ran, every `sites` it changed and the `pin` test, and round two reviews only that delta against the named invariants instead of re-running a full panel. Two distinct findings with the same invariant route to principal design, and a split verdict on one head goes to one adjudication node rather than starting another round. The `code-review` and `deliver` skills state this contract and cite the spec that declares it. (#4017)
- The split adjudicator is a routed read-only agent; the gate uses its verdict after exactly one adjudication run. (#4017)
