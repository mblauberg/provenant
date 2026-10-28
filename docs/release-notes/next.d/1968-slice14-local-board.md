## Additions
- Add `console board` with complete local run and attempt metadata, nullable usage coverage, and filter/root-bound paging below a fixed attempt high-water mark (50 rows by default, 100 maximum). Public cursor checksums detect accidental edits; current generations signal change without invalidating a walk. Damaged observations degrade per row; artefact bodies remain excluded.
