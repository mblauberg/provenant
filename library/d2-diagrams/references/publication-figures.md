# Publication figures

For a D2 figure in a paper, thesis, report or manuscript. The venue, style guide,
figure index, caption and surrounding claim are authoritative.

## Lock the contract

Before drawing, record: the reader question and the single claim; canonical terms
and exclusions from the text; delivery size, colour or grayscale and
accessibility limits; the project's source, render, caption and index locations;
and whether edits, exports and index updates are authorised. Use reader concepts
by default. Filenames, commands and schema fields appear only when
reproducibility or an implementation contract is the figure's purpose; check
the rendered labels, not the D2 identifiers.

## Layout

1. One reading direction: a vertical spine for a true sequence, a directed graph
   for real branching. Do not force a DAG into a decorative stepper.
2. Fold minor detail into the caption or split the figure.
3. Reuse the project's palette and shape grammar. Without one, keep a small role
   set and encode distinctions in shape or label as well as colour.
4. Try the default layout, then `elk` for a dense graph.
5. Include a legend only for distinctions the caption and shapes do not already
   explain.

## Export

```bash
d2 validate diagrams/figure.d2
d2 diagrams/figure.d2 output/figure.svg
```

D2's native PDF export places the PNG render on the page (see
<https://www.d2lang.com/tour/exports/>). That is fine when the project accepts
it. When the venue needs vector shapes and text, convert the SVG with an
installed, project-approved tool, for example:

```bash
rsvg-convert -f pdf -o output/figure.pdf output/figure.svg
pdffonts output/figure.pdf
pdfimages -list output/figure.pdf
```

Reject missing fonts, broken layout or an unexpected full-page raster. Do not
install a converter or write into the manuscript tree without authority.

## Evidence

Inspect the render at delivery size. Build the containing document when
authorised and check page fit, caption, cross-reference and warnings. After a
repair, rerender and repeat the affected checks. A read-only review returns
findings and proposed changes without touching the figure, index or manuscript.
