## Fixes

- Seal nested work membership with host-verifiable child run and attempt IDs. Rewritten IDs are refused before launch and cannot poison a sibling's lookup or launch. Existing child records remain explicitly unindexed without migration.
