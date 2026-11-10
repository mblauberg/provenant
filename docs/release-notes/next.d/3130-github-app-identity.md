## Changes
### Commands

- `land`, `pr status`, `pr watch` and the CI observer can authenticate as a GitHub App installation when `github.json` has an `app` entry for the repository, so they stop drawing on the user's REST quota. Without one they use the user's `gh` login and warn once per process. The App never posts reviews, approvals or comments. See `docs/runbooks/github-workflow.md`. (#3130)
### Lanes and settlement

- New knob `lanes.github_request_budget` (default 100, 0 off). A lane that makes more `gh` requests is stopped with a cause naming the budget, and its settlement records `github_requests`. (#3130)
