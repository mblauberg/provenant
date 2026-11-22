## Fixes
### CLI and install

- `provenant upgrade --verify` gives the smoke graph's review nodes a real
  range: the scratch fixture now commits a base and a fixture change, and the
  launch pins the review base to that commit while the writer still starts at
  `HEAD`. The bundled `smoke` template accepts an optional `diff_base` binding;
  `--verify` and `--dry-run` supply the fixture's base so those reviews have a
  non-empty subject to inspect, while an unbound `diff_base` leaves the review
  nodes unpinned and keeps the normal no-base refusal. A healthy install
  reports `smoke: true` again. (#3798)
