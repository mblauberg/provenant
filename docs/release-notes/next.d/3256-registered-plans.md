## Fixes
### Accounts, quota and routing

- OpenCode account selection follows the registered Go or Zen plan, independent of its key. Quota lists each registered identity once; Go credentials no longer create a synthetic Zen account. Missing or unknown plans retain unknown quota evidence; free-model pacing remains separate (#3256).
