<!-- Modified for Provenant. -->

# Visual system

Derive roles from canonical project tokens and rendered use before proposing
values. Make hierarchy legible through a deliberate combination of size,
weight, colour, spacing, position, and grouping rather than default containers.
Spacing and layout should express relationships, adapt to content, and preserve
useful rhythm; no ratio, grid, or corner treatment is universal.

## Colour and type

Map colour to surface, text, border, action, status, focus, and data roles.
Preserve project-native colour spaces unless the toolchain and migration goal
support another. Test every actual foreground/background pair, including
interaction and forced-colour states. Pure black, white and grey are neither required nor forbidden.

For typography, verify the actual face, available weights, rendering, content,
density, theme, zoom, reflow, and loading behaviour. Dark mode does not imply a
universal weight shift. Establish role, measure, line height, and fallback from
observed needs rather than fixed scales. 16px is a common ergonomic default,
not a universal minimum.

Use tabular numerals (`font-variant-numeric: tabular-nums` or the face's
equivalent) wherever digits update in place or align in columns: counters,
timers, timestamps, currency, and numeric table columns. Align comparable
figures on the right or the decimal point. Proportional figures can stay in
running prose. Verify the face actually ships tabular figures.

## Alignment and corners

Measured alignment can look wrong: asymmetric icons such as a play triangle,
glyph side-bearings, round shapes beside square ones, and icons beside text
whose cap height sits off the box centre. Correct by eye against real content,
with small offsets owned by the component rather than one-off nudges, and
recheck across themes and zoom.

Keep nested corners concentric: the inner radius is the outer radius minus the
padding between them, not the same value repeated at every level. When the
padding meets or exceeds the outer radius, the inner corner may be square. Treat
radius as a token role, not per-component taste.

## Imagery and expression

Use approved real assets before approximations. Check crop, focal point,
resolution, contrast, alternative treatment, loading, and small-screen
behaviour. Apply expressive colour, scale, asymmetry, or texture only when it
supports the approved identity and surface job. When changing intensity, name
one axis, preserve meaning, and verify the result in context.

Source: Vercel, [Web Interface Guidelines](https://vercel.com/design/guidelines).
