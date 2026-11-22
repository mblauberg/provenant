## Fixes

### Pipelines and graphs

- A confirmed, worth-fixing `should-fix` on a default path now starts a repair writer instead of being neither repaired nor blocking. Only `blocking` severity or a blocking class (`acceptance` or `default_path_defect`) contradicts PASS; notes are reported, never repaired. One vocabulary map translates the severity words external reviewers use, and an unrecognised word is warned and kept as a repairable `should-fix` rather than invalidating the review (#3783)
