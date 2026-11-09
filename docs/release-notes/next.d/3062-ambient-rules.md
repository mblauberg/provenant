## Behaviour changes
### Chair, inbox and console

- Chairs now delegate substantial work by default, delegated agents may delegate within their own brief, and agents block on `provenant status --wait` or `provenant pr watch` instead of polling (#3062).
- The review ladder now counts reviewers other than the author, in fresh contexts, rather than "independent" reviewers; the required counts are unchanged, and a delegating agent counts as an author of its children's output (#3062).
