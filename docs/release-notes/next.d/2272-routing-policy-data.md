## Behaviour changes
### Accounts, quota and routing
- Load bundled model policy from embedded rule data, including cache ratios, compaction windows and DeepSeek's peak schedule. Existing configuration and account rules override those defaults. Writer cost advice uses configurable exact model classes and pool order from catalogue data instead of model-name substrings. Default policies for catalogued routes are unchanged.
