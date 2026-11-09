## Changes

### Review and repair

- Reviewers apply one definition of blocking, delivered to every lens: a failed acceptance criterion, a defect on a reachable default path, a breach of a declared hard boundary, or data loss. A finding with no real trigger, or one off the default path that breaches no boundary and loses no data, is a note. Findings state their trigger, reach and whether they were observed or inferred, and propose the fix that removes the cause. A test finding names the production change that would pass unnoticed (#3072).
- The principal scoping a stuck repair now says whether the findings share one flaw, compares the structural fix with the small one and recommends one (#3072).
