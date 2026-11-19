## Fixes

### Waits and CI observations

- `wait --pr` reads a pull request's mergeability before diagnosing missing
  checks. A conflicting head now reports cause `merge_conflict` with a next step
  to merge the base into the branch (`git merge origin/BASE` then push) rather
  than blaming workflow triggers and suggesting an empty commit; a mergeable
  head keeps that trigger diagnosis. UNKNOWN mergeability is reread a bounded
  number of times before choosing. (#3492)
