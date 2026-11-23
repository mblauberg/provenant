## Additions

### Lanes and settlement

- A read-only lane's tree is now a sourced setting: `lanes.read_source` takes `auto` (the default advisory pin), `checkout`, `default_branch` or an explicit ref, with `lanes.read_source_fetch_first` fetching before the ref is read and `--source local|upstream` mapping onto the same resolver. The commit read is named in the warning and in `read_source` on the launch status and result, and the plan node of `implement --plan` reads the launch's `--base`. (#3512, #3360)
