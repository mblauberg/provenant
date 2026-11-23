## Additions

### Lanes and settlement

- A layered `lanes.brief_preamble` file carries standing lane rules, so briefs
  carry only the task. Host, project (`.provenant/config.toml`) and launch
  (`--preamble-file`) layers concatenate in that order with their sources shown;
  a layer may scope itself to writer, review or read-only, and a missing file
  warns while the launch proceeds. Writers' preambles also name the project's
  no-build `verify.static_checks` port entries to run before pushing, without
  refusing the push on failure. (#3498 #3652)
