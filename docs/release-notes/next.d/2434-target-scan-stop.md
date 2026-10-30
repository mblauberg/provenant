## Fixes
### CLI

- Prefix selections stop scanning the run history at the same point whatever the page size, so an ambiguous ID always reports the same bounded list of matches with the same remainder wording, even when a page boundary falls where the scan would stop, and several attempts of one run are deduplicated before the conflict counts the matches (#2434).
