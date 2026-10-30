## Fixes

- A legacy slot-root writer reservation now fences only the attempt it names, so a successor attempt in the same slot is no longer reported as holding the previous attempt's reservation and its temporary files can be collected by cleanup. A slot-root record that cannot be read or decoded is still treated as retained (#2383).
