# Diagram Patterns

## What Good Looks Like

A useful project diagram should:

- explain a method, component boundary, or data flow
- align with the actual code, scripts, artefacts, or claims it depicts
- be readable without a live demo
- avoid jargon-heavy labels when a simpler term will do

## File naming examples

- Main figure sequence: `diagrams/07_system_overview.d2`
- Supporting or exploratory figure: `diagrams/scratch_flow_options.d2`
- Shared imported fragment: `diagrams/shared/<name>.d2`

Use the project's diagram directory and naming convention. The paths above are
examples, not defaults. If the project numbers figures, preserve its ordering.

## Recommended Diagram Shapes By Use Case

Pipeline or workflow:
- build or generation flow
- annotation/review flow
- training/evaluation orchestration

Architecture/container view:
- runtime components
- frontend/backend split
- distributed participants and aggregation

Class or contract view:
- component internals
- key runtime abstractions
- structured artefact contracts

SQL-table view:
- dataset or manifest summaries
- processed record fields worth discussing in prose or an appendix

## Labelling Guidance

- Prefer `Reviewed Dataset` over `Reviewed Dataset Builder Final Output Stage`.
- For reader-facing figures, prefer concepts over filenames, commands, types or
  schema fields. Exact implementation names are appropriate when reproducibility
  or a contract is the figure's explicit purpose. See
  [publication-figures.md](publication-figures.md).
- Follow the surrounding document's terminology and disclosure boundary.

## Example Patterns

Runtime component architecture:

```d2
direction: right

input: "Incoming Request"

engine: {
  label: "Processing Engine"
  window: "Recent Window"
  notes: "Note Memory"
  episodic: "Episodic Memory"
  patterns: "Pattern Index"
  pinned: "Pinned Evidence"
}

backend: "Model Backend"
output: "Prediction"

input -> engine.window
engine.window -> engine.notes
engine.notes -> engine.episodic
engine.episodic -> engine.patterns
engine.patterns -> engine.pinned
engine.pinned -> backend
backend -> output
```

Evaluation flow (reader concepts, not filenames; the schematic still obeys the de-jargon rule):

```d2
inputs: "Input cases"
runner: "Evaluation run"
results: "Results"
stats: "Multi-seed statistics"

inputs -> runner
runner -> results
results -> stats
```

## When To Split A Diagram

Split the diagram when any of these are true:

- more than one main narrative is competing for attention
- labels have to become tiny to fit
- one audience needs the high-level method and another needs low-level contracts
- the same figure tries to show both runtime behaviour and offline artefact lineage

## Cross-Check Before Finishing

- Does the diagram match current code or docs?
- Are the nouns consistent with the surrounding text's terminology?
- Is the flow direction obvious?
- Would a reader understand the main claim in under 15 seconds?
