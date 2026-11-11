# Design defaults

Where the project's design doc is silent, start here and record any departure in
it. When defaults conflict, correctness and recoverability come before speed, and speed
before decoration.

## Action safety

Classify each action before designing its control.

- **Reversible:** act at once and offer an undo that names what it reverses
  ('Undo archive 3 items'). Keep the action in history so undo outlives the toast
  and stays keyboard-reachable.
- **Compensable:** the correction is a new recorded action (void, refund,
  reversal), never an edit of the original.
- **Irreversible:** preview the exact effect (what, how much, to whom), then
  commit. The commit control names its effect ('Send 12 reminders'), never 'OK' or
  'Confirm'. It is not the initial focus and cannot be triggered by implicit form
  submission.

High-stakes actions (moving money, sending outside the product, legal or
regulatory submissions, permission changes) are never optimistic. Show a pending
state until the system confirms, keep draft, pending, done and failed distinct in
text, and make them safe to retry. Confirm only before unexpected irreversible
loss; routine confirmations teach people to click through.

## Feedback and states

- A toast confirms the person's own action and is never the only home of an error
  or an undo. System state (degraded, read-only, expired session) is a banner that
  persists. Critical messages never auto-dismiss. A disabled control says why.
- Define states in this order: loading, error, empty, content; also permission
  denied and extreme content.
- First-use empty says what will appear and offers the one creating action.
  Filtered-to-zero says so and offers to clear filters, not to create.
- Errors sit at the point of failure, keep everything entered and never show a
  bare code (wording: `writing`).
- Degraded: show working content with a banner, gate affected actions and say why.
  Offline: say what still works, queue or refuse writes explicitly, mark cached
  data with its as-at time.

## Modality, tables, keyboard

- Lightest container wins. Editing a record in a work tool defaults to a sheet or
  side panel with the list visible. Never nest dialogs. Shareable or reloadable
  state (filters, tabs, pagination, open panel) lives in the URL.
- Tables: declare each column's kind once (text, number, amount, date, status,
  identifier); it sets alignment, formatter and sort. Numbers use tabular figures
  and consistent decimals, align to the end edge, and nothing is centred. At
  narrow widths keep the table (frozen first column, scroll) rather than cards
  when comparison matters.
- One action registry (id, verb label, shortcut, scope, availability, permission)
  feeds the command menu, shortcuts, context menus and tooltips so labels cannot
  drift. Never override browser, OS or assistive-technology shortcuts. Focus is
  always visible, never hidden by sticky chrome.

## Accessibility, colour, tokens

- WCAG 2.2 AA is the floor across every criterion, not contrast alone. For each
  area name how it was tested or record it not tested. Compute contrast numerically
  on rendered colour pairs, including opacity and compositing.
- Each status hue has one meaning everywhere and is never reused as brand or
  decoration. Never rely on colour alone: a negative amount shows its minus sign,
  an overdue item says 'Overdue'.
- Components use semantic tokens only; a raw value in a component is a finding.
  Design light and dark separately.

## Content and trust

- Buttons are verbs naming the result ('Archive client'). One verb per lifecycle
  step, used identically in buttons, menus, the command menu, notifications and
  history. One capitalisation rule, sentence case by default.
- Consequential data carries trust cues: an as-at time on computed or cached
  figures; per-change history (actor, human or system, time, before and after,
  source); links from a figure to its records; clear draft and final marking.
