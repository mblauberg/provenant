## Fixes
### CLI and install

- `provenant upgrade --verify` gives the smoke graph's review nodes a real
  range: the scratch fixture now commits a base and a fixture change, and the
  launch pins the review base to that commit while the writer still starts at
  `HEAD`. The bundled `smoke` template also accepts a `diff_base` binding, so
  every smoke review has a non-empty subject to inspect instead of refusing
  the launch. A healthy install reports `smoke: true` again. (#3798)
