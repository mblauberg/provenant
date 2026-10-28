# D2 CLI Usage

Assumes `d2` is installed and available on `PATH`. The examples use `diagrams/diagram.d2` as a
placeholder path; substitute your own.

## Core Commands

Format a diagram:

```bash
d2 fmt diagrams/diagram.d2
```

Check formatting in scripts or CI-style workflows:

```bash
d2 --check diagrams/diagram.d2
```

Validate syntax and references:

```bash
d2 validate diagrams/diagram.d2
```

Render to SVG:

```bash
d2 diagrams/diagram.d2 diagrams/diagram.svg
```

When a venue requires vector shapes/text, go via SVG and an installed converter.
D2's native PDF is PNG-derived and can still be appropriate for multi-board or
raster-acceptable workflows:

```bash
d2 diagrams/diagram.d2 diagrams/diagram.svg
rsvg-convert -f pdf -o figures/diagram.pdf diagrams/diagram.svg
pdffonts figures/diagram.pdf
pdfimages -list figures/diagram.pdf
```

Watch during iterative editing:

```bash
d2 --watch diagrams/diagram.d2 diagrams/diagram.svg
```

Use ELK for denser graphs:

```bash
d2 --layout elk diagrams/diagram.d2 diagrams/diagram.svg
```

List available themes:

```bash
d2 themes
```

The hosted playground is an external disclosure surface. Use it only with
explicit authority for the diagram's contents:

```bash
d2 play diagrams/diagram.d2
```

## Practical Defaults

- Default export: `svg`
- Document or manuscript attachment: follow the venue profile; use SVG plus a
  verified converter when vector shapes/text are required
- Slide deck or document image export when vector is not suitable: `png`
- Default layout engine: `dagre`
- Try `elk` for crowded dependency or architecture views

## Suggested Local Loop

```bash
d2 fmt diagrams/diagram.d2
d2 validate diagrams/diagram.d2
d2 diagrams/diagram.d2 diagrams/diagram.svg
```

## Notes

- `d2` defaults to SVG output if no output path is given.
- `--watch` opens a local browser view unless browser opening is disabled.
- Stop run-owned watch processes after use.
- Do not install D2, a layout plugin or a converter without dependency/tooling
  authority.
- Native PDF behaviour is documented at <https://www.d2lang.com/tour/exports/>.
- Keep command examples concrete and project-relative when writing instructions for other agents.
