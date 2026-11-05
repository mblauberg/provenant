## Fixes
### Pipelines and graphs

`review-council` no longer fails at synthesis with "upstream finding F1 needs exactly one disposition" when a verifier reuses short finding ids across lenses and omits the composite prefix from `propagates`. A marker resolves when it is a colon-delimited suffix of one finding's marker, and an ambiguous one still leaves the finding independent with a warning. 1.6.3 restored the council only on routes that reached synthesis without this shape.
