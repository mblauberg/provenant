## Breaking changes
### Lanes and settlement

- **Drain writer lanes before upgrading.** Writer lanes now use a private Git directory, so lanes started by an earlier release keep their old grants. An interrupted older attempt can resume only if it has private objects; otherwise start a fresh run (#2131, #2158).
