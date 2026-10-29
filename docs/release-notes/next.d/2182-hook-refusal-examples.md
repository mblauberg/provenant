## Behaviour changes

- Every refusal from the Claude Bash guard and the Git pre-push hook now shows an accepted example in one line, such as `git push origin my-topic` for a default-branch force push or `git fetch origin && git merge origin/my-topic` for a non-fast-forward push. Dynamic command names, `sleep` and `kill` are not refused any more (#1949), so those refusals no longer exist to explain (#2182).
