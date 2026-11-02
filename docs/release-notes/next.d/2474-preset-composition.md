## Fixes
### Graph presets

- Review shapes govern lens lists only; large `big-batch` lane lists expand in full. Omitted lenses are recorded and warned about (#2474).
- Nested role indices resolve per child copy, injected read roots are optional for children, required typed parameters name missing values, and node titles use concrete IDs unless overridden. Preset inspection now uses a human table and sections (#2465).
- Added a read-only `scope` preset with expansion-time refusal of tree-write overrides. Scratch-only runtime confinement remains incomplete because read-only attempts also receive provider and tool grants (#2370).
