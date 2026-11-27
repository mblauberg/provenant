## Behaviour changes

### Pipelines and graphs

- An admitted subject's `policy_source` is now a typed provenance source (`{layer, origin}`) instead of a prose string. It names the layer that chose the pin and the exact file, key or command-line flag that supplied it; the wait reports the compiled profile, host config file or host-project config file with `FILE#KEY` as its origin, and a `--head`/`--pr-head-policy` pin names the launch flag. Diagnostics print it as `LAYER (ORIGIN)`. (#4021)
