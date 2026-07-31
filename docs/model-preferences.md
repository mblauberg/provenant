# Model preferences

Suggestions, not rules. Nothing here is enforced by a schema or a gate, and no
script parses this file. It exists so preferences can be written in a sentence,
changed in a sentence, and read by whoever is about to dispatch a leg.

Read it before choosing a model. Prefer what it says. Depart from it when the
work calls for something else, and say why in the receipt.

## What is enforced, and what is not

`config/model-routing.json` is the hard catalogue and the only thing that
decides what is *admissible*. It maps a task class to an alias — `flagship`,
`workhorse`, `scout` — and each alias to the models a family can serve it with.
Those aliases are deliberately not version-pinned: that indirection is what lets
a model be swapped without touching a single call site, and it is what the
harness actually routes on.

This file sits above that. It names concrete, version-bearing models, because
that is the level real preferences live at. It never widens what the catalogue
admits, and it must never be turned into something that narrows it either. A
preference that becomes a gate has stopped being a preference.

`config/model-preferences.json` remains machine plumbing for coarse family
affinity and fair spreading. Do not put version-bearing preferences there; they
change too often and carry no room for the reason.

One mechanical detail matters when editing this file. Where an alias in the
catalogue lists several models, **the order is the resolution order**: the first
is what an automated dispatch gets, and the rest stay admissible. So a
preference written here only becomes the automatic default if the catalogue
array is ordered to match. Change both together. A preference that contradicts
the array is not a preference, it is a stale note.

## Current preferences

**Token-heavy legwork goes to OpenAI, and the workhorse there is
`gpt-5.6-luna` at raised effort.** Reading a lot to produce a report, exhaustive
inventories, mechanical sweeps: these are cheaper there and Claude's budget is
better spent on judgement. Luna's price was cut by roughly 80% in July 2026,
which makes Luna at `high` or `xhigh` the best value in the family for
high-token work; go to `max` when a leg genuinely deserves it. `gpt-5.6-terra`
stays admissible but is no longer the preferred default.

That is the old standing wish made real by the price cut, and the mechanical
caveat that blocked it still holds: effort is fixed per task class, so ordering
Luna first in the catalogue array would buy Luna at *medium*, a downgrade
rather than the trade intended. The catalogue order is therefore left alone on
purpose. Ask for Luna and the raised effort explicitly at dispatch, and record
the pair in the receipt. A cheap model with the effort dial up beats a dearer
model at medium.

**`gpt-5.6-sol` stays the OpenAI flagship, reserved for critical and
high-stakes slices.** Give it the work that is genuinely hard or where a miss
is expensive; everything below that now belongs to Luna at raised effort.
Getting this backwards wastes both.

**Anthropic minds are for judgement, not volume.** Keep Opus and Fable for
chairing, adjudication, synthesis and critical review, and reach for them less
often on lower-stakes tasks: Haiku and Sonnet are not priced well enough to be
the cheap alternative, so menial and high-token slices route to Luna instead.
Where workhorse work must stay Anthropic, prefer Opus at low or medium effort
over Sonnet at a higher one. The catalogue lists Opus under
`anthropic.aliases.workhorse` so this is a real option rather than a
flagship-only escape hatch.

**Orchestration stays with Anthropic**, at flagship and high effort. Decomposition,
synthesis and final calls are the chair's job.

**Gemini 3.1 Pro for writing style, naturalisation and polish passes — not for
core changes.** It is chosen for voice, not for reasoning. Use it to make prose
read naturally; do not hand it the logic. `gemini-3.6-flash` is the cheap,
genuinely different family for cross-family review legs, and is reachable at
`-high`, `-medium` and `-low`.

**Critical review has no fixed family.** The cross-family obligation is relative
to whoever chairs the run, so the right second family depends on the first. Do
not write a standing preference here; pick the family that is *not* the author's
and record it.

## Reaching Gemini

Invoke it as `agy --add-dir DIR --model "Gemini 3.6 Flash (High)" -p PROMPT`.
The model flag is `--model`, not `-m`, and the value is the display name shown
by `agy models`. Never pass `--dangerously-skip-permissions`; `agy` prints that
suggestion on every denial, and following it hands Gemini every tool at once.

Headless mode cannot prompt, so it auto-denies anything not already allowed.
The three tool families behave differently, and the differences were measured,
not inferred:

- **Reads work only through `--add-dir`.** A `read_file(...)` allow-rule with a
  glob does not match; a read outside the added directories is denied even when
  a `read_file(/some/prefix/**)` rule appears to cover it. Add the directory and
  reads inside it just work. This is the mechanism to rely on.
- **`write_file` matches exact literal paths only.** Globs never match, in
  either the `/tmp` or the `/private/tmp` spelling. Allowing a write means
  writing that one full path into `permissions.allow` before the run.
- **`command(NAME)` matches the binary and admits any arguments.** `command(rg)`
  permits `rg` with arbitrary flags. Rules of this shape are far broader than
  they look — `command(sed)` includes `sed -i`, which writes files.

The practical consequence: do not ask Gemini to write its own output file.
Generate any diff yourself, pass the directory with `--add-dir`, and redirect
the CLI's stdout to capture the review. That path needs no allow-rules at all.

## Changing this file

Edit it directly. It needs no migration, no baseline and no regenerated fixture,
which is the entire point. If a preference here starts being treated as binding,
that is a bug in the caller, not a reason to formalise the file.
