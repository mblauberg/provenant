# `scripts/provenant_lint.py`

Checks branch names and commit messages against the doctrine in
[`setup-repo`](../../skills/setup-repo/references/doctrine.md). Owner:
`scripts/provenant_lint.py`. Exit 0 passes or skips, 1 reports violations,
2 is a usage error. Problems print to stderr.

```sh
scripts/provenant_lint.py branch [NAME]      # default: the current branch
scripts/provenant_lint.py commit-msg FILE    # works directly as a git commit-msg hook
```

Hook: `.git/hooks/commit-msg` containing `exec scripts/provenant_lint.py commit-msg "$1"`.

## Rules

Branch: `<type>/<area>-<slug>`, `proto/<area>-<slug>` or
`land/<yyyymmdd>-<slug>`; at most 48 characters; slug of 2-5 kebab-case words;
no run vocabulary (batch, wave, round, lane) or agent and model names.
`dependabot/...` passes. The default branch (`main`, `master` or
`tools.default_branch`) and a detached HEAD are skipped with exit 0.

Commit message, after dropping comment lines (honours `core.commentChar`; kept
under `commit.cleanup=verbatim`), the scissors tail and HTML comments:

- an empty or comment-only message fails;
- session markers (`wave 12`, `lane-3`, `WP4`, run ids) fail;
- agent attribution (`Co-Authored-By:`, `Generated with <agent>`) fails;
- a closing keyword with an issue (`Closes #n`, `fixes #n`) fails; use
  `Refs #n`, since the pull request owns closure.

Attribution matches only the `Co-Authored-By:` trailer and a `Generated with <agent>` footer line, never prose. Attribution and closing-keyword checks ignore fenced blocks and inline code spans; session markers are checked on the raw text.

Merge messages (`Merge branch|pull request|...`) and `Revert "..."` pass.

## Config

Read leniently from `tools` in `.agents/fabric-policy.json` by
`tools_policy()` in `skills/orchestrate/scripts/fabric_policy.py`. A missing
or malformed file, or a key of the wrong type, falls back to the default
(a malformed key prints a warning). Unknown keys are ignored.

| Key | Default | Meaning |
| --- | --- | --- |
| `tools.default_branch` | `"main"` | Branch that `lint branch` skips |
| `tools.lint.areas` | `[]` | Allowed `<area>` words; empty allows any lowercase word |
| `tools.lint.forbid_attribution` | `true` | Reject attribution lines |
| `tools.lint.forbid_closing_keywords` | `true` | Reject closing keywords in commits |

The session-marker and closing-keyword patterns are Python ports of Sealle's
JS regexes; `tests/test_provenant_lint.py` checks them against `node` over a
synthetic corpus, and over Sealle's history when that checkout is present.
