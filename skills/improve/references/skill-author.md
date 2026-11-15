# Skill author

Create or materially revise a skill under an explicit write grant.

## Build

1. Choose the appropriate owner with the least standing cost: a project rule, a skill, a tested script, an
   app, or a separately versioned plugin. Split only for a distinct
   trigger, authority, artefact or gate; merge skills that compete for the same
   request.
2. Keep project-only policy in the project. A project skill that only points at
   a global skill is redundant; one that departs from it lists each departure
   under `Project overrides:`.
3. Run the task without the skill, then with it, in a fresh context without
   leaking the intended answer. Keep only lines that changed the result.
4. Run the validation the target repository names; otherwise say which checks
   did not run.

## Description

One line saying what the skill is for, in the words a user would say, trigger
terms first. Add a `Not for` clause only where two skills would genuinely both
take the same request, and name the skill to use instead; a bare negation does
not route.

## Body

The outcome, the few rules a capable agent would get wrong, and the commands.
Defaults an agent may override with a reason, except at real boundaries.
Nothing the always-loaded instructions already say. Link another skill by name
instead of copying its rules. Hard rules sit early; depth goes in references
one hop away. Leave out logs, dated history, exhaustive flags and generic
advice. Write rules as mechanism and reason, phrased positively. Version facts
that will date go in a dated reference.

Contract tests cover only invariants a script or CI gate enforces; never assert
wording.

## Third-party material

Copied or closely paraphrased text needs its source URL, commit, retrieval date
and licence, with the notice kept. Never run an imported installer, hook or
binary to inspect it. Metadata and tool lists cannot widen authority.
