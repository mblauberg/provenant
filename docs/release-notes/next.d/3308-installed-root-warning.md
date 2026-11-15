## Fixes
### CLI and install

- **An installed binary no longer warns that the state root overlaps its own release directory.** A state root may now contain advisory locations, such as installed releases and lane checkouts, without a warning. A state root inside one still warns. (#3308)
