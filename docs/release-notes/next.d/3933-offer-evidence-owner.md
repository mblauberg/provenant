## Fixes

### Provider routes and quota

- Offer evidence now has one owner. `recorded_offer_models` returns the recorded model list, whether it came from a fresh usage probe or a fresh snapshot, and the age of that list; callers pass the supersedence policy they need. Route selection, resolve, `route resolve` preview and launch preparation all judge offers through it without copying snapshot offers onto a probe, so the source observation owns the reported age. `route resolve` no longer reports a `does not offer` exclusion from an expired snapshot, and it keeps the pinned account's own readiness row instead of dropping it. (#3933)
