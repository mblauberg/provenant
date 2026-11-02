# Product design defaults

These are the defaults a product designer applies to any application. A
project's own design doc overrides them. Where it is silent, start here and
record any departure in that doc. Locale, currency, date formats, brand and
domain vocabulary belong to the project, not to these defaults. When two
defaults conflict, use the project's ranking; without one, correctness and
recoverability come before speed, and speed before decoration.

## Action safety

Classify every action before designing its control.

- **Reversible.** Act at once and offer an undo that names what it reverses
  ("Undo archive 3 items"). Record the action in history, so undo stays
  reachable by keyboard after any toast has gone.
- **Compensable.** The correction is a new, recorded action, such as a void,
  refund or reversal. It never edits the original.
- **Irreversible.** Preview the exact effect (what, how much, to whom), then
  commit. The commit control names its effect ("Send 12 reminders"), never
  "OK" or "Confirm". It is not the default focus and Enter alone does not
  trigger it.

High-stakes actions (moving money, sending outside the product, legal or
regulatory submissions, permission changes) are never optimistic. They show a
pending state until the system confirms, keep draft, pending, done and failed
visibly distinct in text, and are safe to retry. Ask for confirmation only
before unexpected, irreversible loss: routine confirmations teach people to
click through them.

## Feedback

Pick the channel by scope and urgency.

| Channel | Use for | Rule |
|---|---|---|
| Inline | A field, row or control | Sits beside what it describes |
| Toast | Brief confirmation of the person's own action | Short; never the only home of an error or an undo; announced politely |
| Banner | Page or system state: degraded service, read-only mode, expired session | Persists until resolved or dismissed |
| Notification | Events that happen while the person is elsewhere | Lands in an inbox or panel they can revisit |
| Dialog | A critical decision that must block | Rare; see Modality |

Critical messages never auto-dismiss. A disabled control says why it is
disabled. A significant completion links to its result.

## States every surface defines

Render in this order: loading, error, empty, content. Also check permission
denied and long or extreme content.

- **Loading.** Show nothing for waits expected under about a second. Delay a
  spinner by roughly 150 to 300 ms and, once shown, keep it long enough not to
  flash. Use a skeleton that matches the final layout for page-shaped loads, so
  content does not shift on arrival. Use determinate progress when the duration
  is known; past about ten seconds, let the work continue in the background.
  Keep the rest of the surface usable.
- **Empty.** First use says what will appear and offers the one action that
  creates it, using the same noun. Filtered to zero says so and offers to
  clear the filters, never to create a first item. An empty section inside a
  page stays compact.
- **Error.** Say what happened, why if known, and what to do next, at the point
  of failure. Keep everything the person entered. Never blame them. A code
  never stands alone: a support reference may follow the message in secondary
  text. On a form, show each error beside its field, summarise long forms at
  the top in the same words, and move focus to the first error on submit.
- **Degraded.** When the primary content still works, show it with a banner and
  in-context notes rather than an error page. Never imply data loss, hide
  counts that cannot be computed and keep critical controls available.
- **Offline.** Say what still works, queue or refuse writes explicitly, show
  sync state, and mark cached data with its as-at time.

## Modality

Choose the lightest container that keeps the person's context.

- **Inline:** edit one value in place.
- **Popover:** a short, non-blocking choice anchored to its trigger, such as a
  picker or filter.
- **Sheet or side panel:** inspect or edit a record while the list stays
  visible. This is the default for editing records in a work tool.
- **Dialog:** a short blocking decision or focused task, titled by the task,
  with an obvious way out. Never nest dialogs. Guard unsaved work on close.
- **Page:** a multi-step flow, a long form, or anything that deserves its own
  URL and back button.

Filters, tabs, pagination and open panels that someone might share or reload
belong in the URL.

## Data-dense tables

- **Column kinds.** Declare each column's kind once (text, number, amount,
  date, status, identifier). The kind sets alignment, formatter, figure style
  and sort comparator. Numbers and their headers align right with tabular
  figures and consistent decimals; text aligns left; nothing is centred. A unit
  shared by the whole column goes in the header.
- **Density.** Density is a table-level token set (row height, cell padding,
  type step) with at least a comfortable and a compact option. Header and
  toolbar heights follow the row height. Targets stay at least 24 px in compact
  mode, and a coarse pointer raises them whatever the setting.
- **Selection and bulk actions.** Selection uses a checkbox column with a
  tri-state select-all. Selecting rows replaces the toolbar with a bulk-action
  bar that states the count, and select-all across pages states its scope. Bulk
  actions follow the action safety classes.
- **Reading.** Only the sorted column shows a sort indicator. Sticky headers
  never cover the focused row. Totals are visually separated and never wrap.
  Where comparison matters, a narrow width keeps the table (frozen first
  column, horizontal scroll) rather than converting rows to cards.

## Keyboard

- **One action registry.** Each action has an id, verb label, shortcut, scope,
  availability rule and permission. The command menu, shortcuts, context menus,
  toolbar tooltips and the shortcut sheet all read it, so labels and
  availability cannot drift. Menus and tooltips show each shortcut beside its
  label so people learn it.
- **Shortcuts.** Never override browser, operating-system or
  assistive-technology shortcuts. Single-key shortcuts have an off switch, or
  are remappable or active only while their component has focus.
- **Focus.** Focus is always visible and never hidden by sticky chrome. It
  enters an opened overlay, stays inside a modal one and returns to its trigger
  on close. After a deletion it moves to the next sensible item. Composite
  widgets (grids, menus, listboxes, tabs) follow the WAI-ARIA Authoring
  Practices patterns.

## Accessibility

WCAG 2.2 AA is the floor across every criterion, not contrast alone. For each
area, name how it is tested, or record it as not yet tested.

| Area | Typical test |
|---|---|
| Text and non-text contrast | Computed from tokens in an automated check |
| Keyboard operation, focus order, focus visible and not obscured | Scripted keyboard paths in a browser test, plus a manual pass |
| Target size (24 by 24 px) and alternatives to dragging | Measured in the rendered page |
| Reflow at 320 px, text resize and text spacing | Rendered at those settings and inspected |
| Names, roles, states and live announcements | Automated rules for structure; a screen-reader pass for flows |
| Reduced motion | Rendered with reduced motion preferred |
| Consistent help, redundant entry, accessible authentication | Review |

Automated scanners find only part of the failures. The evidence rules in
[craft](craft.md) say what a capture can and cannot prove.

## Colour and meaning

Colour carries status, and each status hue has one meaning everywhere. Sign,
label and icon carry direction and meaning: a negative amount shows its minus
sign, an overdue item says "Overdue". Never rely on colour alone. A status hue
is never reused as a brand accent or decoration, and chart categories use a
palette separate from status colours. Selection, focus and status survive
forced-colours mode because they also use borders, outlines or text.

## Tokens and themes

- **Layers.** Primitives hold raw ramps. Semantic tokens name roles: surface,
  text hierarchy (primary, secondary, tertiary), border, status and focus.
  Components use semantic tokens only, so a raw value in a component is a
  finding.
- **Ramps.** Build ramps in a perceptual space such as OKLCH, with one
  lightness curve across hues, so contrast follows step distance (for example,
  text at least a fixed number of steps from its surface). Verify the ratios
  numerically anyway, and keep values inside the sRGB gamut or supply
  fallbacks.
- **Themes.** Design light and dark separately, each with its own ramps and
  elevation, rather than inverting one. Check each theme with increased
  contrast and forced colours.
- **Surfaces and motion.** Name surfaces by role, and let chrome recede below
  content. Keep a small set of duration and easing tokens. Frequent actions do
  not animate layout, and reduced motion turns movement into a fade or an
  instant change.

## Content design

- **Voice and tone.** The voice stays fixed and the tone follows context: calm
  and exact for errors and risk, brief for confirmations, plain for newcomers
  and outside readers, precise domain terms for expert users.
- **Word list.** Keep one canonical noun per object and one verb per lifecycle
  step. Use them in buttons, menus, the command menu, notifications, history
  and any visible API. Buttons are verbs that name the result ("Archive
  client"), with an ellipsis when more input follows.
- **Capitalisation and spelling.** Apply one capitalisation rule everywhere;
  sentence case is the usual default. Spelling follows the project's locale.

## AI content and trust cues

- **AI-generated or suggested content** is marked where it appears, in text as
  well as any icon, and kept visibly distinct from data people entered or the
  system computed. Show its basis or source where possible. A suggestion
  becomes part of the record only through a person's accept or edit, and
  history records who accepted it.
- **Consequential data** carries trust cues: an as-at time on computed or
  cached figures; a history of each change (actor, human or system, time,
  before and after, and source such as the interface, an import or an
  integration); links from a figure to the records behind it; and clear draft
  and final marking.

Informed by Apple's Human Interface Guidelines, Stripe, Linear, GitHub Primer,
IBM Carbon, Shopify Polaris, Vercel's web interface guidelines, the GOV.UK
Design System, Radix Colors, Material Design 3 and WCAG 2.2.
