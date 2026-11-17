## Fixes

### Chair, inbox and console

- Graph ask polling treats a missing inbox request as pending until deadline default applies, rather than reporting stored state corruption. Corrupt requests continue to report state corruption detail. (#3301)
