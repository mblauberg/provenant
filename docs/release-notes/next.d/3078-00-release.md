## Breaking changes
### CLI and install

- **Drain lanes and land or park 1.x work before installing 2.0.** The first default install creates a fresh v2 state root and copies only `config.toml` and `host-id`; 1.x run state and the old install receipt are not migrated. Review the retained v1 root and old client configuration, and remove anything left over when no longer needed. An identified custom 1.x root is refused, including with `--yes`: use the default v2 root or a fresh custom root and configure it manually. Install 2.0 with the bootstrap `./install.sh`. Select your harness again with `--harness` if autodetection is unsuitable. See the [install runbook](../../runbooks/install.md#first-20-install-after-1x). (#3078)
