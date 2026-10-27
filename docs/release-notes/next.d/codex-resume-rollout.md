## Fixes

- Codex resumes copy retained session directories into the new attempt without
  carrying over indexes that point to the previous attempt's rollout. This
  prevents resume from trying to read session files outside its sandbox (#1790).
