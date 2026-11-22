## Fixes

### Chair, inbox and console

- `provenant console overview` reports `runs: empty` with zero counts on a host with no run store, matching `console board` and `console tree`, instead of reporting run state as unavailable. The console no longer says "Run state is unavailable here" when the run scan is merely partial: it reports that counts may be incomplete
