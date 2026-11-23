## Behaviour changes

### CLI and install

- `config show` reports the source of each setting leaf: its layer and origin.
  Port-table leaves such as `verify.remote` and `verify.base` report their own
  source rather than the port-level label, and `--full` also shows the layers
  each leaf shadowed. (#3777)
