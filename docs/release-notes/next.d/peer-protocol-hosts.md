## Behaviour changes

- Host entries report `protocol` only when known. Mismatched peers remain in
  the list with `available: false`, their protocol and `reason: "peer_version"`.
