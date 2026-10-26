## Additions

- `provenant output RUN/TASK/ATTEMPT --trace` now also lists the grants applied
  at launch: write grants, read-only exceptions and the Git grants digest (a
  `grants` object in `--json`). An attempt launched before grants were
  recorded lists none (#1564, #1571).
