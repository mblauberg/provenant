---
name: uml-diagrams
description: "Use for requirements-spec UML in PlantUML: use-case, package, activity, include/extend, and swimlane diagrams. Not for D2, Mermaid, C4, Structurizr, or unrelated PlantUML; use the matching diagram owner."
---

# Requirements-Spec UML Diagrams

Create or review requirements-specification UML diagrams. The target project's
diagram profile, terminology, document structure and source/render ownership are
authoritative. Use the neutral rules here only where the project is silent.
Prefer PlantUML source (`.puml`) and inspect a render before finalising.

## Platform choice

1. PlantUML first when the project has no established diagram tool: use cases,
   packages, actors, include/extend, swimlanes, decisions, forks, joins, and
   finals auto-route reliably.
2. Preserve an established project tool. For manual SVG/Python, read
   `references/manual-rendering-quality.md`.
3. Otherwise use Excalidraw or D2 only when explicitly requested; route D2 to
   `d2-diagrams`.

## Workflow

1. Read the relevant guide:
   - package overview or per-package use case diagram: `references/use-case-and-package-rules.md`
   - activity diagram: `references/activity-rules.md`
   - before finalising: `references/common-mistakes.md`
   - high-stakes deliverable: `references/multi-agent-review-loop.md`
2. If no project template exists, start from the matching skill template:
   - `templates/use_case_package_template.puml`
   - `templates/use_case_diagram_template.puml`
   - `templates/activity_diagram_template.puml`
3. Use stable aliases consistent with the project.
4. Lint with the skill-relative script (the linter is heuristic, not a project
   conformance oracle):
   ```bash
   python3 "$(provenant root)/skills/uml-diagrams/scripts/lint_plantuml_diagram.py" path/to/diagram.puml --type auto
   ```
5. Render:
   ```bash
   python3 "$(provenant root)/skills/uml-diagrams/scripts/render_plantuml.py" path/to/diagram.puml --format svg
   ```
   Set `PLANTUML_JAR=/path/to/plantuml.jar` and `--format png` to render through a
   local PlantUML JAR instead of the default installation.
6. Inspect the image using `references/rendering-quality-checklist.md`; fix and
   re-render until the objective gates pass. For a read-only request, write only
   to an assigned run-owned temporary output and report proposed source changes.

## UML correctness and consistency

- `<<include>>`: base -> included, arrowhead at included.
- `<<extend>>`: extension -> base, arrowhead at base; record its condition in
  the diagram or linked requirement according to the project profile.
- No solid association directly between two use cases; only include, extend, or generalisation.
- A package-only overview does not also carry the detailed per-package use-case
  view; split conceptual levels unless the project explicitly combines them.
- Cross-package references use the project's traceability convention; do not
  invent package numbers, section numbers or table names.
- Transitive includes are not direct includes; trace chains using the project's
  requirements convention.
- Activity diagrams show workflow for one use case; use swimlanes when roles/systems participate.
- Alternate flows that reconverge need merge nodes. Separate terminating flows may have separate finals, but never stacked finals.
- Omit empty swimlanes and place actions with the participant that performs them.
- Keep actors, the project's requirements register, detailed use-case
  descriptions and diagram associations consistent.
- Leave enough canvas margin so labels do not clip.

For high-stakes review, load `orchestrate`; runtime routing and the harness risk
tier choose reviewers. Objective lint, render, document-build and traceability
evidence outrank reviewer votes.

## Resources

- `references/plantuml-patterns.md` and `references/srs-fit.md`.
- `references/rendering-quality-checklist.md`.
- `scripts/lint_plantuml_diagram.py`, `scripts/render_plantuml.py`.
