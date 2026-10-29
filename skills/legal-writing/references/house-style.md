# House style (legal overlay)

Load `natural-writing` first for Australian English spelling, punctuation,
dates and numbers. This file adds the legal-specific layer: forum labels,
defined terms, citations in running text, structure and tone. It follows
mainstream Australian usage (the Australian Government Style Manual, the
Macquarie Dictionary and the current AGLC) except where marked **(skill
default)**, which is a drafting convention, not a forum rule. Project
instructions, current official requirements and the supplied form or template
override it. Verify the forum's current form, practice direction and filing
specification before relying on any default.

## Terms and labels

- Use `filed`, `served`, `lodged`, `affirmed`, `sworn`, `annexure`, `exhibit`,
  `applicant`, `respondent` and similar labels only where the forum or form
  uses them.
- A party without a lawyer is a `self-represented litigant` or `litigant in
  person` (the forum's current term), never `pro se` outside quoted source text.
- Use `judgment` for a court judgment.
- Plain terms in new drafting: `under` for `pursuant to`, `before` for `prior
  to`, `after` for `subsequent to`, `use` for `utilise`, `phone` for
  `telephone`, `about` or the precise relation for `in relation to`. Keep the
  older word inside an exact quotation, statute or rule title, form field,
  defined term, case name or source extract.

## Punctuation

The em-dash ban is lint-enforced here (`provenant check writing <paths> --rules
legal` reports a filing blocker on U+2014); the en dash is a warning only. The
spaced hyphen is not caught by lint: use it only where the governing template or
sibling titles require it, never as a clause separator. Single quotation marks
follow AGLC; set a quotation over about three lines as an indented block without
quotation marks.

## Defined terms and citations

- Define a short form once in parentheses, then use it consistently:
  `the respondent, [FULL NAME] ('[SHORT FORM]')`, in plain text, not bold. Do
  not capitalise generic references (`the parties`, `a tribunal member`).
- Italicise case names and legislation titles; keep the jurisdiction tag and
  pinpoint outside the italics. Use the current AGLC edition for formal
  authority citations unless the forum prescribes otherwise (see
  `source-boundary-and-citations.md`).
- Money in figures with a separator and two decimals where exactness matters
  (`$14,132.18`). Preserve competing figures exactly. A date in an official
  form field follows that form's format.

## Structure (neutral defaults, template governs)

- Headings are short, stable and descriptive. Lettered topic headings (`A.
  Purpose`) are the skill default for affidavits and submissions; the letter is a
  label and never resets paragraph numbering.
- If the forum requires numbered paragraphs, follow its scheme. Otherwise number
  substantive paragraphs `1` to `N` continuously, one proposition or small
  factual cluster each **(skill default)**.
- One sub-paragraph hierarchy: `(a)`, then `(i)`. Use block sub-paragraphs for
  three or more items, relief or anything cross-referenced; keep a two-item
  aside inline. Write the literal `(a)` marker in Markdown source.
- Bold headings only **(skill default)**; no bold for party short forms, money
  or argument.
- A cited paragraph number is the rendered continuous number. Adding or removing
  a whole paragraph breaks every later reference: prefer sentence-level edits or
  fix every reference. Verify the rendered result, not the Markdown shape.

## Sentences and tone

Answer, issue, request or relief before background. Prefer active voice; passive
is proper where the actor is unknown or less important than the act. Keep the
actor, act and object together. Average about 20 words; treat a sentence over
about 24 words or with three or more list items as a split-or-tabulate prompt,
not a filing rule. State what is sought, proved or disputed positively, and keep
honest negatives where absence is the fact (non-service, non-admission, no
order, no finding, an issue under appeal). Keep `alleged`, `disputed` and `not
admitted` where they define status. Tone is formal, restrained and direct:
never promotional, theatrical, sarcastic, chatty or American. Do not smooth a
sourced fact into a generic summary; keep the date, amount, document and exhibit
label that make it provable.
