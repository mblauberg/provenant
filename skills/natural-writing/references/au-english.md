# Australian English (always-on default)

`natural-writing` defaults to Australian English for any prose without a
project style guide, product, API, quoted source, or repository convention
that requires another variant. `engineering-writing` and `legal-writing`
inherit this default and layer only their own domain overlay (forum wording,
LaTeX identifiers) on top; they do not restate it. The academic exceptions
(`per cent` in running prose, preserved citation keys, minimal capitalisation)
are in [academic-prose.md](academic-prose.md).

This file skips the basics a model already gets right by default (`-ise` not
`-ize`, `-our` not `-or`, `-yse` not `-yze`, the `licence`/`license` and
`practice`/`practise` noun-verb split) and keeps only the rules that are
genuinely easy to miss or to get backwards.

Preserve the source spelling exactly inside quotations, identifiers, package
names, API fields, config keys, filenames, error strings, citation keys, form
fields, and copied source. Australian English governs the prose you author,
not text you quote.

**Locale note:** this default is Australian English. US English is a future
`locale` branch, not built yet; a locale flag could switch the default later.

## Non-obvious spelling and word choice

- `-re`, not `-er`: `centre`, `metre`, `litre`, `fibre`, `theatre`, `calibre`.
  Keep `-er` in words that never took `-re`: `parameter`, `diameter`,
  `filter`, `buffer`, `header`, `compiler`, `container`, `register`.
- Doubled `l` on inflection: `travelling`, `modelling`, `cancelling`,
  `labelling`, `signalling`, `levelled`. The single-`l` American forms
  (`traveling`, `modeling`, `labeled`) are wrong here.
- `-logue`, not `-log`, in prose: `catalogue`, `dialogue`, `analogue`. Keep
  `log` and `dialog` where they are identifiers, CLI subcommands, or UI widget
  names (a `dialog` component, a `changelog` file).
- `program`, not `programme`, in every sense (a program, a training program,
  to program). `programme` is British and dated in Australian technical and
  general prose.
- `judgment`, not `judgement`, for a court or considered decision
  (`engineering judgment`, `the Court's judgment`); `judgement` is acceptable
  general-prose spelling but pick `judgment` for consistency across a
  document.
- Keep `-ment` where standard: `acknowledgement`, `enrolment` (single `l`),
  `instalment` (single `l`), `fulfilment`.

## Numbers, dates and money

- Dates in prose: day, then the month spelled out, then the year, no comma,
  no ordinal suffix: `2 July 2026`, not `2nd July` or `July 2, 2026`.
  Numeric dates are day/month/year (`2/7/2026`); never write the American
  `m/d/yyyy` in prose, since it is ambiguous to a non-Australian reader.
  ISO `YYYY-MM-DD` stays correct and preferred in logs, filenames, and
  anywhere sort order or machine parsing matters. Do not "Australianise" a
  timestamp.
- `per cent` (two words) in running prose; `%` in tables, captions, UI text,
  metrics and other numeric/technical contexts.
- Spell out numbers under ten (or, in some domains, under two); use numerals
  from ten up, except at the start of a sentence. Hold one convention per
  document.
- Currency: symbol then numerals, no space (`$500,000`); Australian dollars
  are the default, write `A$` or `AUD` only where a second currency is in
  play.

## Punctuation

- No em dash (`—`, U+2014) in any output. Replace by function: an aside
  becomes parentheses or commas; an explanation becomes a new sentence or a
  colon before a list or definition; a contrast becomes `but`, `yet`, a
  semicolon, or a new sentence; a range becomes prose words (`from ... to`) or
  an unspaced en dash in tables and captions. Do not "fix" a banned em dash by
  reproducing its dramatic pivot with a colon, spaced hyphen, or semicolon:
  that moves the tell, it does not remove it.
- En dash (`–`, U+2013) only for a closed numeric, date, or page range
  (`pp 12–18`, `2024–2026`), no surrounding spaces. Do not use a spaced en
  dash as a habitual parenthetical.
- The **spaced hyphen** (` - `) escapes em-dash bans and simple lint checks,
  so watch for it by eye. It is acceptable in a title or heading where
  sibling documents already use it; avoid it as a clause separator in body
  prose, where it reads as dash overuse.
- Semicolons have two reliable jobs: separating list items that themselves
  contain commas, and occasionally binding two short balanced clauses in
  deliberate antithesis. Default to a full stop for two ordinary independent
  clauses; prose peppered with clause-joining semicolons reads as machine
  rhythm.
- A colon introduces a list, a genuine explanation that completes the lead
  clause, or a label gloss. It never splices on a second free-standing
  sentence or reproduces an em-dash pivot.
- Single quotation marks for quoted words and field names; double only for a
  quotation inside a quotation. Keep the exact quote characters when
  reproducing code or copied strings (curly quotes break copy-paste in code).
- No serial (Oxford) comma by default; add one only where it removes
  ambiguity. Consistency within a document matters more than the rule.
- Sentence case for headings by default; match the repository if it already
  holds Title Case consistently.

## Hyphenation

- Never hyphenate an `-ly` adverb plus adjective or participle, in any
  position (`fully connected layer`, `randomly sampled subset`, not
  `fully-connected`, `randomly-sampled`). This is the single strongest
  American/AI over-hyphenation tell across every writing domain in this
  family; keep it at zero. The only fixed exceptions are `fully-fledged` and
  `fully-fashioned`.
- Hyphenate a compound modifier only before the noun it modifies
  (`a low-rank adaptation`, `a rate-limited endpoint`); open it after the
  noun or in predicate position (`the adaptation is low rank`).
- Prefer the closed form for established compounds: `dataset`, `baseline`,
  `runtime`, `preprocessing`, `online`, `codebase`, `database`, `email`,
  `hyperlink`, not their hyphenated ancestors.
- Do not stack three-or-more-word hyphen chains or `versus`-joined modifiers
  (`urban-versus-rural`, `pre-and-post-intervention`); recast as a
  prepositional or relative clause. This is usually a noun tower wearing
  hyphens; see the anti-AI taxonomy's noun-stacking section.

## Terminology

Prefer the Australian or plain term over its American or bureaucratic
equivalent, unless a quote, form, or API requires otherwise:

| Prefer | Avoid |
|---|---|
| mobile / mobile phone | cell / cellphone |
| autumn | fall |
| public holiday | national holiday / federal holiday |
| GST | sales tax / VAT |
| ABN, ACN | EIN / tax ID |
| postcode | zip code |
| CV | resume / résumé |
| enrol, enrolment | enroll, enrollment |
| whilst / while | (both fine; prefer `while`) |

## Checklist

1. `-re`, doubled-`l` inflections, `-logue`, `program`/`judgment` correct.
2. Dates spelled out in prose, ISO in logs/data, no American `m/d/yyyy`.
3. `per cent` in prose, `%` in tables and values.
4. No em dash; en dash only for closed ranges; no spaced hyphens as clause
   separators; single quotation marks.
5. No `-ly`-adverb hyphens; closed modern compounds; no stacked hyphen
   chains.
6. Terminology matches the Australian/plain preference table.
7. Source spelling preserved only inside quotes, identifiers and copied
   material.
