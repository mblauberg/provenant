## Behaviour changes

### Lanes and settlement

- Stale-state, optional-evidence and preference checks gain `policy.<CODE>` rules that default to warn and can be set to `refuse` for strict mode; each warning names the setting source. The rules are listed under `policy_rules` in `config/harness-knobs.json`. Call sites move over in follow-up changes. #3371
