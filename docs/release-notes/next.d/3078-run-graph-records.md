## Breaking changes

### Pipelines and graphs

- Graph nodes accept `prompt` as their one prompt spelling. Node-level `brief` is refused with the replacement field. Run and graph records decode host-owned state through typed fields, so malformed values produce decode errors. (#3078)

### Fabric

- Upgrade connected hosts together. Peer graph transport now carries typed settlement and output metadata and refuses incompatible protocols. (#3078)
