## Breaking changes
### Git and safety

- **Drain active deliveries before upgrading, or re-save their receipts with the new binary.** An in-flight receipt with a downgrade needs current factors, a PR subject with `repository` set to `HOST/OWNER/REPO` instead of an opaque ID, and recorded chair downgrade authority; saving recomputes the derived minimum and records the PR's actual merge-base-to-head paths, including both names of renames. Old derived values and downgrade records are not migrated; remove `risk.changed_paths` from save requests and fetch the PR base and head locally (#3081).
- Review boundary floors follow effective configuration, so owners can lower or remove bundled defaults. Credential delivery and Git custody helpers receive their boundary floors by default; non-PR downgrades are refused because they have no diffable commits, while ordinary tier selection is unchanged (#3081).
