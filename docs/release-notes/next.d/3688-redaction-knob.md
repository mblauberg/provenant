## Behaviour changes

### Lanes and settlement

- Redaction now defaults to `off`, keeping model and route metadata visible. Set `redaction` to `secrets` to mask credentials at egress or `full` to also mask local text. `config show` reports the setting and source. (#3688)
