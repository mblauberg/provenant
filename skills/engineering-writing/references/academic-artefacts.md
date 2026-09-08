# Academic artefacts

Use this reference for thesis and paper LaTeX command and layout invariants.
Academic prose, chapter structure, research voice, whole-work review and AI
tells belong to `natural-writing`.

Diagnosis is read-only. Edit only assigned files. Do not restructure a work,
change a bibliography or replace citations without explicit authority.

## LaTeX invariants

Preserve exactly unless explicitly asked otherwise:

- `\cite{...}`;
- `\Cref{...}`, `\cref{...}`, `\ref{...}` and `\autoref{...}`;
- `\label{...}` and project-defined command arguments, including result macros;
- equations, symbols, table alignment, figure paths, bibliography commands,
  glossary and acronym commands.

Do not rewrite a macro argument for style. A macro argument can be a contract
with generated artefacts. Keep cross-references specific, preserve the existing
`\Cref`/`\cref` split, and flag unclear notation rather than changing symbols.
Result macros remain locked: do not replace them with guessed values or hide
unresolved tokens.

### Common hazards

- deleting braces around macros, changing `_` in labels or replacing `~` in
  non-breaking references;
- introducing unescaped `%`, `_`, `&` or `#`;
- changing table alignment while editing prose;
- treating LaTeX `---` as a visible style problem instead of repairing the
  sentence punctuation.

Keep the non-breaking tilde before `\ref`, `\cite` and `\eqref`. Preserve a
non-breaking space between numbers and units or percentages; `\cref` and
`\Cref` already supply it.

### Markup-safe editing checklist

- Every citation key is preserved or explicitly verified.
- LaTeX commands and arguments are unchanged.
- Result macros are not converted into numbers.
- Cross-references still point to the same artefact.
- Unsupported claims are flagged rather than smoothed away.

## File-editing workflow

1. Read surrounding paragraphs, not only the target sentence.
2. Preserve comments, fences, labels and macro structure.
3. Make the smallest coherent prose edit.
4. Re-read for markup and claim drift.
5. Run targeted checks when available.
