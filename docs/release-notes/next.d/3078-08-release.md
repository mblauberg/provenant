## Breaking changes
### Lanes and settlement

- **Build wait settings are removed.** Remove the old build and slot wait settings; busy build slots and resource admission warn by default. Launch policy reports its source and override, and strict enforcement is configurable. Writer scope remains writable across the worktree, with out-of-scope paths reported at settlement. (#3078, #3208, #3275)
