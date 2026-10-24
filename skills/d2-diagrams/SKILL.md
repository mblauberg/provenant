---
name: d2-diagrams
description: "Use to create, validate, render or export D2 diagrams, including fixed-layout and vector-PDF figures. For Mermaid, Graphviz or PlantUML diagrams, use engineering-docs."
---

# D2 diagrams

Use D2 when fixed layout, repeatable exports or publication quality justify
maintaining source plus a rendered asset. The project's diagram conventions,
source/render owners and terminology are authoritative. Where the project is
silent, prefer native Mermaid for GitHub READMEs and living operational docs.
Simplify or split an overloaded Mermaid graph before converting it; a tool
change cannot fix mixed conceptual levels. A review or validation request is
read-only unless edits or exports are explicitly authorised.

## Publication figures (anything in a written document)

For a paper, thesis, report or manuscript:

1. Match labels to the intended reader. Use implementation names only when
   exact reproducibility is the figure's purpose.
2. Match the target venue's output requirements. D2's native PDF path is
   PNG-derived; when vector shapes/text are required, render SVG and use a
   project-approved converter, then inspect the PDF.

Read [publication-figures.md](references/publication-figures.md) before editing
a publication figure.

## Where To Put Things

- Use the project's diagram directory and naming convention when writes are
  authorised; otherwise use an assigned run-owned temporary path.
- Export only when requested or required by the authorised workflow.
- Never overwrite unrelated Mermaid or Structurizr artefacts.

## Default Workflow

1. Choose the reader question and diagram type.
2. Draft concise labels and stable identifiers.
3. For authorised edits, run `d2 fmt`; for read-only review, use `d2 --check`.
4. Run `d2 validate` when the installed CLI supports it. If D2 or a required
   converter is unavailable, report the skipped check rather than installing it.
5. Render SVG by default to an authorised path; use PNG only when raster is
   required.
6. Open the render and inspect labels, edges, grouping, whitespace and page fit.

## Defaults

- Prefer `dagre` for directed flows; try `elk` for dense architecture.
- Group related systems; distinguish roles only when it aids the argument.
- Keep detail in prose, follow the project's language convention (otherwise
  Australian English), and exclude real PII.

## Syntax And CLI References

- For core D2 syntax, read [references/syntax.md](references/syntax.md).
- For local CLI usage, read [references/cli.md](references/cli.md).
- For diagram-shaping and naming guidance, read [references/diagram-patterns.md](references/diagram-patterns.md).

## Guardrails

- Prefer plain nodes and edges; avoid themes, icons and flourishes by default.
- Do not commit batches of exports without authority.
- Split crowded diagrams instead of forcing one canvas.
- `d2 play` uses a hosted viewer. Do not invoke it for private or undisclosed
  source without explicit external-disclosure authority.
- Close run-owned watch processes and remove only assigned temporary outputs.
- For high-stakes review, load `orchestrate`; let runtime routing choose review
  families and adjudicate findings against render/build evidence.

## Official Sources

- D2 repository: https://github.com/terrastruct/d2
- D2 tour/docs: https://d2lang.com/tour/
