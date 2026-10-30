## Fixes
### CLI

- `output`, `resume`, `cancel` and `receipt` now stop reading the run history as soon as a prefix resolves to more than one distinct target, instead of paging through every lane. Ambiguous selections report the same error wording over a bounded list of the matches (#2409).
