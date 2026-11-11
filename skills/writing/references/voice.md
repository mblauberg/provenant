# Voice and Australian English

## Default voice

Plain and mildly casual, exact where it counts. Docs and reports take it at a
lighter dose; commits, error text and release notes stay terse. Formal
register (see [academic.md](academic.md)) is the exception, and a supplied
sample outranks everything here.

> Line coverage was 8/8 in both runs, yet all seven survivors were on the
> checks in lines 6, 9 and 12 of classify(). The baseline only uses scores 60,
> 75, 90 and -1, so none of its tests lands on a threshold.

Every claim sits on a number or a line, and `so` carries an observation to its
consequence.

- Mostly declarative sentences. Confident where the evidence is direct; one
  qualifier only where it is genuinely limited.
- Contractions (`doesn't`, `there's`): freely in email and chat, about one
  every few paragraphs in documents. Plain verbs over formal ones: `tell them
  apart`, `get thrown out`.
- Reports stay impersonal, or use first person only for actions taken (`I ran
  ...`), never for opinions or feelings. Correspondence is written as the
  sender.
- Anchor each claim to a number, name, line, date or result. Keep the one
  number a claim rests on even when a table holds it; a redundancy pass that
  strips it leaves the claim unanchored.
- Mix sentence lengths: a short verdict beside a longer one that chains the
  reasoning.
- Tables for parallel facts, prose for reasoning, bullets only for genuinely
  parallel items. In long documents, run-in bold labels (`MR1.`,
  `Requirement: not satisfied.`) beat a heading per paragraph.
- Cut what the reader could infer: no prose restating a table, no caption that
  repeats its heading.
- Human texture comes from rhythm and plain verbs, never planted typos, slang
  for effect, rhetorical questions or invented detail.
- To match a supplied sample, take sentence-length spread, paragraph density,
  openings, punctuation, pronouns, certainty, warmth and bluntness. Borrow
  habits, not signature phrases. Dialect stays unless standardising is asked.

## Australian English

Default for prose you author. A model already gets `-ise`, `-our`, `-yse` and
the `licence`/`practice` noun-verb split right, so only the easy-to-miss rules
follow.

- **Quotations and identifiers keep their source spelling**: quotes, package
  names, API fields, config keys, filenames, error strings, citation keys,
  form fields, titles. A project style guide, product, API or recipient that
  needs another variant (US English for a US venue, say) overrides this
  default.
- `program`, not `programme`. `judgment`, held consistent within a document.
  `acknowledgement`, `enrol`/`enrolment`, `instalment`.
- Keep `log`, `dialog` and `changelog` where they are identifiers or widget
  names.
- Dates: `2 July 2026`, no ordinal, no comma. Numeric dates are day/month/year,
  never `m/d/yyyy`. ISO `YYYY-MM-DD` stays in logs, filenames and anywhere
  sort order matters.
- `per cent` in running prose; `%` in tables, captions, equations and UI text.
  Spell out numbers under ten, numerals from ten up. Pick one per context.
- Currency is `$500,000`, Australian by default; `A$` only beside a second
  currency.
- Omit the hyphen after an `-ly` adverb (`randomly sampled subset`). Hyphenate
  a compound modifier only before its noun (`a rate-limited endpoint`; `the
  endpoint is rate limited`). Recast three-word chains as a clause.
- Single quotation marks, double only inside a quotation; code and copied
  strings keep their exact characters. Omit the serial comma unless it removes
  ambiguity. Sentence-case headings unless the repository uses Title Case.

### Punctuation

Replace an em dash by function: an aside takes parentheses or commas; an
explanation takes a new sentence or a colon; a contrast takes `but`, `yet` or
a new sentence; a range takes `from ... to`.

- An en dash is for closed numeric ranges only, unspaced: `pp 12–18`. Not a
  habitual parenthetical.
- A spaced hyphen slips past em-dash checks; avoid it as a clause separator in
  body prose.
- A semicolon separates list items that contain commas, or binds two short
  balanced clauses. Two ordinary clauses take a full stop.
- A colon introduces a list or completes the lead clause. It never splices on a
  second free-standing sentence.
