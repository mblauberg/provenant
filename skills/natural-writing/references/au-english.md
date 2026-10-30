# Australian English (always-on default)

`natural-writing` writes Australian English for any prose that has no project
style guide, product, API, quoted source or repository convention requiring
another variant. `engineering-writing` and `legal-writing` inherit this
default and add only their domain overlay (forum wording, LaTeX identifiers);
they don't restate it. Academic work follows it too; the points that matter
there are in [academic-prose.md](academic-prose.md#register).

A model already gets the basics right (`-ise` not `-ize`, `-our` not `-or`,
`-yse` not `-yze`, the `licence`/`license` and `practice`/`practise`
noun-verb split), so this file keeps only the rules that are easy to miss or
to get backwards.

Australian English governs the prose you author, not text you quote. Keep the
source spelling exactly inside quotations, identifiers, package names, API
fields, config keys, filenames, error strings, citation keys, form fields and
copied source. Foreign spelling in titles stays as printed.

**Locale note:** the default is Australian English. US English is a future
`locale` branch, not built yet; a locale flag could switch the default later.

## Spelling and word choice

| Pattern | Write | Notes |
|---|---|---|
| `-re` | `centre`, `metre`, `litre`, `fibre`, `theatre`, `calibre` | Words that never took `-re` keep `-er`: `parameter`, `diameter`, `filter`, `buffer`, `header`, `compiler`, `container`, `register`. |
| Doubled `l` on inflection | `travelling`, `modelling`, `cancelling`, `labelling`, `signalling`, `levelled` | The single-`l` forms (`traveling`, `modeling`, `labeled`) are wrong here. |
| `-logue` in prose | `catalogue`, `dialogue`, `analogue` | Keep `log` and `dialog` where they're identifiers, CLI subcommands or UI widget names (a `dialog` component, a `changelog` file). |
| `program` | a program, a training program, to program | `programme` is British and dated in Australian technical and general prose. |
| `judgment` | `engineering judgment`, `the Court's judgment` | `judgement` is acceptable in general prose; pick `judgment` throughout a document for consistency. |
| `-ment` | `acknowledgement`, `fulfilment`, `instalment` (single `l`), `enrol`/`enrolment` (single `l`) | Not `enroll`, `enrollment`. |

## Terminology

Prefer the Australian or plain term unless a quote, form or API requires the
other.

| Prefer | Avoid |
|---|---|
| mobile, mobile phone | cell, cellphone |
| autumn | fall |
| public holiday | national holiday, federal holiday |
| GST | sales tax, VAT |
| ABN, ACN | EIN, tax ID |
| postcode | zip code |
| CV | resume, résumé |

`while` and `whilst` are both fine; prefer `while`.

## Numbers, dates and money

| Item | Rule | Example |
|---|---|---|
| Date in prose | Day, month spelled out, year; no comma, no ordinal suffix | `2 July 2026`, not `2nd July` or `July 2, 2026` |
| Numeric date | Day/month/year. Never the American `m/d/yyyy` in prose: it's ambiguous to a non-Australian reader | `2/7/2026` |
| ISO date | `YYYY-MM-DD` stays correct, and preferred, in logs, filenames and anywhere sort order or machine parsing matters. Leave timestamps alone | `2026-07-02` |
| Per cent | `per cent` (two words) in running prose; `%` in tables, equations, captions, UI text and metrics. Pick one per context and hold it | `12 per cent of runs`; `12%` in a table |
| Small numbers | Spell out numbers under ten (in some domains, under two); numerals from ten up, except at the start of a sentence. One convention per document | `three seeds`, `12 seeds` |
| Currency | Symbol then numerals, no space. Australian dollars by default; write `A$` or `AUD` only where a second currency is in play | `$500,000` |

## Punctuation

An em dash (U+2014) never appears in output. Replace it by function:

| The dash was | Use instead |
|---|---|
| an aside | parentheses or commas |
| an explanation | a new sentence, or a colon before a list or definition |
| a contrast | `but`, `yet`, a semicolon or a new sentence |
| a range | `from ... to` in prose; an unspaced en dash in tables and captions |

Rebuilding the dramatic pivot with a colon, spaced hyphen or semicolon moves
the tell without removing it, so recast the sentence.

| Mark | Rule |
|---|---|
| En dash `–` (U+2013) | Closed numeric, date or page ranges only, with no spaces: `pp 12–18`, `2024–2026`. Not a habitual parenthetical. |
| Spaced hyphen ` - ` | It slips past em-dash bans and simple lint checks, so watch for it by eye. Fine in a title or heading where sibling documents already use it; avoid it as a clause separator in body prose, where it reads as dash overuse. |
| Semicolon | Two jobs: separating list items that contain commas, and occasionally binding two short balanced clauses in deliberate antithesis. Two ordinary independent clauses take a full stop; clause-joining semicolons throughout read as machine rhythm. |
| Colon | Introduces a list, an explanation that completes the lead clause, or a label gloss. It never splices on a second free-standing sentence. |
| Quotation marks | Single for quoted words and field names; double only for a quotation inside a quotation. Reproduce code and copied strings with their exact characters (curly quotes break copy-paste in code). |
| Serial comma | Omit by default; add one only where it removes ambiguity. Consistency within a document matters more than the rule. |
| Headings | Sentence case by default, with initial capitals only for formal names; match a repository that already holds Title Case consistently. |

## Hyphenation

| Rule | Write | Avoid |
|---|---|---|
| Leave out the hyphen after an `-ly` adverb, in any position. It's the strongest American/AI over-hyphenation tell in every writing domain here, so hold it at zero. Only `fully-fledged` and `fully-fashioned` stay | `fully connected layer`, `randomly sampled subset` | `fully-connected`, `randomly-sampled` |
| Hyphenate a compound modifier only before its noun; open it after the noun or in predicate position | `a low-rank adaptation`, `a rate-limited endpoint`; `the adaptation is low rank` | `the adaptation is low-rank` |
| Prefer the closed form for established compounds | `dataset`, `baseline`, `runtime`, `preprocessing`, `online`, `codebase`, `database`, `email`, `hyperlink` | their hyphenated ancestors |
| Recast chains of three or more words and `versus`-joined modifiers as a prepositional or relative clause; they're usually a noun tower wearing hyphens | | `urban-versus-rural`, `pre-and-post-intervention` |
