## Behaviour changes

### Lanes and settlement

- Default `lanes.cargo_target` to `shared`, giving lanes in the same project a shared warm Cargo target directory under the host cache instead of private copy-on-write clones. Set to `private` or an absolute directory to override. (#3442)
