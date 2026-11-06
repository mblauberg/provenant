## Breaking changes
### Pipelines and graphs

- **Finish pending graphs and drain live lanes with the previous release before upgrading.** The new release refuses old pending state even when the upgrade used `--yes`; port user and repository presets to templates by hand.
- **Automatic preset rules and pipeline experiments are removed.** Remove `domains.fabric.pipelines.experiment` from configuration; stale keys produce a warning. Historical child stores remain on disk, but their records drop out of statistics.
