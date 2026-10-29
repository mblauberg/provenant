## Behaviour changes
### Git and safety

- Every refusal from the Claude Bash guard and the Git pre-push hook now shows an accepted example in one line, such as `git push origin my-topic` for a default-branch force push. Dynamic command names, `sleep` and `kill` are no longer refused (#2182).
