# Acknowledgements

This file records intellectual and practical influences. Formal copyright and
licence obligations live in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)
(the prose index), the top-level [NOTICE](NOTICE) (Apache-2.0 §4(d)
attributions) and the licence texts in [LICENSES/](LICENSES); this is credit,
not the legal record.

## Adapted or redistributed components

- [Impeccable](https://github.com/pbakaus/impeccable) by Paul Bakaus is the
  basis of `ui-ux-design` and its private `runtime/ui-evidence` and
  `runtime/ui-live` implementations (Apache-2.0). The live runtime retains
  [modern-screenshot](https://github.com/qq15725/modern-screenshot) (MIT).
  Historical UI/UX research used data from
  [UI UX Pro Max](https://github.com/nextlevelbuilder/ui-ux-pro-max-skill/tree/539d52c424c18a14626601a0494ff70561b86d8b/.claude/skills/ui-ux-pro-max/data)
  (MIT); that CSV data is not shipped in the active skill.
- [Matt Pocock's `grill-me`](https://github.com/mattpocock/skills/blob/62f43a18177be6ec82da242e59ffbc490a4c22ea/skills/productivity/grill-me/SKILL.md) is the basis of
  `grill-me` (MIT).
- [Skill Optimizer](https://github.com/hqhq1025/skill-optimizer/tree/f10e5d85371f72841459493ed750f45ed9afa99d/skills/skill-optimizer) by hqhq1025 is
  the basis of `skill-craft`'s audit branch (MIT).
- [Superpowers](https://github.com/obra/superpowers) by Jesse Vincent informed
  the approach in `tdd`, `diagnose` and `skill-craft`.

## Independently implemented influences

- The agentic lifecycle was informed by Google's *The New SDLC With Vibe
  Coding* [whitepaper](https://www.kaggle.com/whitepaper-the-new-SDLC-with-vibe-coding).
  The implementation, authority model and receipts in this repository are
  original Provenant work.
- The independent-panel pattern is inspired by
  [Andrej Karpathy's LLM Council](https://github.com/karpathy/llm-council).
  No council code is incorporated.
- The structural-review lens is inspired by Cursor Team Kit's
  [Thermo-Nuclear Code Quality Review](https://github.com/cursor/plugins/blob/main/cursor-team-kit/skills/thermo-nuclear-code-quality-review/SKILL.md).
  No plugin code is incorporated.
- `caveman` is an original, safety-bounded cross-agent rewrite informed by
  [Julius Brussee's Caveman](https://github.com/JuliusBrussee/caveman), audited
  at commit `0d95a81d35a9f2d123a5e9430d1cfc43d55f1bb0`. No upstream savings claim,
  hook, compressor, worker agent or substantial wording is incorporated.

Thank you to these authors and maintainers for making their work inspectable and
reusable.
