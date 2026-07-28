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

**Token-heavy legwork goes to OpenAI.** Reading a lot to produce a report,
exhaustive inventories, mechanical sweeps: these are cheaper there and Claude's
budget is better spent on judgement. `gpt-5.6-terra` is the default workhorse
and should stay it.

The standing wish is `gpt-5.6-luna` **at xhigh effort** as a workhorse — cheap
model, effort turned up, rather than a more expensive model at medium. That is
not expressible yet: effort is fixed per task class, so ordering Luna ahead of
Terra in the catalogue would buy Luna at *medium*, which is a downgrade rather
than the trade intended. Until a task class can carry an effort range, ask for
Luna and a raised effort explicitly when a leg suits it, and leave the default
alone.

**`gpt-5.6-sol` is the stronger OpenAI model, not the faster one.** Give it the
slices that are genuinely hard. `luna` is for trivial and mechanical work.
Getting this backwards wastes both.

**Prefer Opus at low or medium effort over Sonnet.** For workhorse work where
the family lands on Anthropic, Opus at a lower effort tends to beat Sonnet at a
higher one. The catalogue lists Opus under `anthropic.aliases.workhorse` so this
is a real option rather than a flagship-only escape hatch.

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

`agy` reads files under `--add-dir` without extra permission. It cannot run
shell commands or write files in headless mode — those need `command(...)` and
`write_file(...)` allow-rules in `~/.gemini/antigravity-cli/settings.json`, and
adding them is the operator's call. Generate any diff yourself and redirect the
CLI's stdout rather than asking Gemini to write its own output. Never pass
`--dangerously-skip-permissions`.

## Changing this file

Edit it directly. It needs no migration, no baseline and no regenerated fixture,
which is the entire point. If a preference here starts being treated as binding,
that is a bug in the caller, not a reason to formalise the file.
