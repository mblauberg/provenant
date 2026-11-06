## Breaking changes
### Lanes and settlement

- **Remove `[domains.guard]` from `config.toml`.** Shell commands no longer pass through a command-rule table. Writer lanes approve provider tools and can execute programs created in their checkout; filesystem confinement, credentials, object verification and host authority gates remain enforced (#2964).
