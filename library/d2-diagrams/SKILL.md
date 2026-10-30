---
name: d2-diagrams
description: "Use to author, validate, render or export D2 diagrams, including fixed-layout and publication figures (SVG, vector PDF). Not for Mermaid or Graphviz (engineering-docs defaults to Mermaid) or for deciding where a diagram lives (engineering-docs)."
---

# D2 diagrams

D2 earns its upkeep when a fixed layout, repeatable export or publication
quality justifies maintaining source plus a rendered asset. Otherwise use
native Mermaid, which GitHub renders. Split an overloaded graph before
converting it: a tool change cannot fix mixed conceptual levels. Project
diagram conventions, owners and terminology win. A review request is
read-only unless edits or exports are authorised.

## Workflow

1. Name the reader's question and the one claim the diagram supports. Keep one
   conceptual level; split rather than shrink labels.
2. Draft stable identifiers on the left and short visible labels on the right.
   Plain nodes and edges; style only where a distinction carries meaning.
3. Check with the installed CLI (verify with `d2 --version`; commands below are
   as of 0.7.1, checked 30 October 2026). `d2 fmt file.d2` rewrites in place, so
   run it only when edits are authorised. `d2 fmt --check file.d2` is the
   read-only format check and `d2 validate file.d2` the read-only syntax check;
   `d2 --check` still renders. If `d2` or a converter is missing,
   report the skipped check; do not install it.
4. Render SVG by default: `d2 file.d2 file.svg`. Use `--layout elk` for dense
   directed graphs, `dagre` otherwise. Use PNG only when raster is required.
5. Open the render and inspect clipping, overlap, edge-label ownership,
   whitespace and grayscale meaning. Parsing is not visual verification.
6. Compare every visible noun and arrow with the caption and source. Semantic
   correctness outranks symmetry: arrows match the claimed direction, future
   work does not feed current evidence, failure paths do not pass through a
   success result.

## Placement

Use the project's diagram directory and naming; otherwise a run-owned temporary
path. Export only when requested. Never overwrite unrelated Mermaid or
Structurizr artefacts, and do not commit export batches without authority.

## Guardrails

- `d2 play` uses a hosted viewer: never for private or undisclosed source
  without explicit disclosure authority.
- Stop run-owned `--watch` processes; remove only assigned temporary outputs.
- Exclude real personal data from labels. Follow the project's language
  convention, otherwise Australian English.
- For high-stakes review, use `orchestrate`; adjudicate reviewer findings against
  render and build evidence.

## Publication figures

For a paper, thesis, report or manuscript, read
[publication-figures.md](references/publication-figures.md) first. The venue,
style guide, caption and surrounding claim are the profile; do not change terms,
claims or generated-file ownership.

D2 documents: <https://d2lang.com/tour/> and <https://github.com/terrastruct/d2>.
