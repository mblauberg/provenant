## Fixes

- Refuse lane-chosen child and continuation run IDs before creating host chair associations, using the existing derived-ID scheme. Chair indexing seals the validated request despite concurrent lane rewrites. Retained legacy host rows remain readable and replayable; root replay repairs only the original chair binding.
