## Fixes

### CLI and install

- Observational policy warnings now carry the layer and knob that set the rule as their source, and are shown once per state change to a person. A `--json` envelope keeps every warning, including repeats, and never marks a key as shown, so two launches in one state root still list `WRITER_CENSUS` in both envelopes while stderr shows it once. The outward contract declares the envelope's `warnings` field as evolving, and a workspace source check keeps `"warning: "` writes in the binary output seam. (#3778)
