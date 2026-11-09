## Breaking changes
### Git and safety

- An in-flight receipt with a downgrade needs a re-save with the new binary to record current factors, chair authority and the PR's actual merge-base-to-head paths, including both names of renames. Remove `risk.changed_paths` from save requests; fetch the PR base and head locally when paths have not yet been recorded. Receipts without a downgrade continue to load and review offline (#3081).
- Review boundary floors follow effective configuration, so owners can lower or remove bundled defaults. Credential delivery and Git custody helpers receive their boundary floors by default; non-PR downgrades are refused because they have no diffable commits, while ordinary tier selection is unchanged (#3081).
