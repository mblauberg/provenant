---
name: prototype
description: "Use for an authorised, timeboxed throwaway build that answers one feasibility question. Not for production code, settled requirements, or debugging known breakage; use implement, scope, or diagnose."
---

# prototype: spike to learn, then throw it away

Some uncertainty is cheaper to build through than to talk through. A
prototype exists to answer ONE named question; the deliverable is the
answer, never the code. A retained design-comparison prototype that a project
keeps under its own register is not a spike; that project's rules govern it.

## Contract (agree before writing code)

1. **The question**: one sentence, falsifiable. "Can X stream 10k rows
   without buffering?" not "explore X".
2. **Timebox**: default 30 to 60 min of agent work. Hit the box with no answer
   → that IS a finding (question too big; split it or grill further).
3. **Kill criteria**: what result answers the question either way.
4. **Where it lives**: an authorised, manifest-owned scratch dir outside
   production source. Never import it from real code or touch external systems,
   credentials or user data without separate authority.

## Rules

- Build only the checks needed to answer the question. Production quality gates
  do not apply to disposable scratch code; evidence, authority and containment do.
- **Cheat aggressively**: hardcode data, stub auth, fake the network. Only
  the question's variable needs to be real.
- **One question per spike.** A second question mid-spike → note it, finish,
  spike it separately if it still matters.
- **Harvest, then retire.** Put the finding and evidence in the owning spec,
  decision record or scoping thread when artifact authority exists. Remove only
  paths proven run-owned and explicitly authorised; otherwise quarantine or
  hand them back with a named cleanup action.
- **Prototype code never "graduates" to production.** Productionisation routes
  through `scope` when decisions changed, then `implement`; rebuild the smallest
  production solution with the appropriate test/refactor method.

## Harvest format (into the owning doc/thread)

```markdown
**Spike:** <question>  (YYYY-MM-DD, timebox 45m)
**Answer:** yes/no/partial — <one paragraph, numbers where measured>
**Evidence:** <command run, output, or measurement>
**Implication:** <what the spec/decision now says because of this>
```

## Red flags

- "The spike works, let's ship it" → scope any changed decision, then use
  `implement`; do not wire the spike into production.
- Spike touching production files or secrets → wrong dir, stop.
- No timebox agreed → not a spike, just unscoped wandering.
- Answer known from docs/a search in <10 min → didn't need a spike.
