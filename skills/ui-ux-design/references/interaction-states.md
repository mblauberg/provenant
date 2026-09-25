<!-- Modified for Provenant. -->

# Interaction and states

Model the user's goal as a compact state matrix: state, trigger/event, guard,
feedback, entry/exit, success, failure, recovery, and interruption. Make
impossible transitions explicit. Require every applicable state, not the same
fixed list for every element. Consider default, hover, focus, active, selected,
disabled, loading, empty, partial, success, error, permission, offline,
conflict, and stale data only where the control or flow can reach them.
Every interactive control answers activation with immediate press feedback,
before any async result, scaled to its frequency per [motion](motion.md).

Use semantic controls and preserve keyboard, pointer, touch, and assistive
technology paths. Focus must remain visible and ordered. Overlays require a
verified accessible name, dismissal policy, background interaction policy,
focus entry, containment when modal, and restoration target. Native popover
provides useful built-in invoker, focus, and dismissal semantics, but still
verify the accessible name, chosen type, focus entry and restoration,
background behaviour, browser support, and pattern-specific keyboard model. Modal focus
containment must be proven, not assumed from the element name.

Async feedback should identify what is happening without blocking unrelated
work. Choose latency feedback, cancellation/back behaviour, acknowledgement
scope, retry, and stale-response handling deliberately. Preserve user input
across recoverable failures. Associate field errors using `aria-invalid` and
`aria-describedby`, and avoid a disruptive layout shift when messages appear.
Errors state what happened, what remains safe, and the next action.

Update optimistically when an action is likely to succeed: show the result at
once, reconcile with the response, and on failure roll back visibly with the
reason and the user's input preserved. Never present a pending result as
confirmed where the difference matters to the user. Reserve blocking spinners
for actions that cannot safely be assumed to succeed.

Prefer immediate execution with reliable undo over confirmation. When
retention, expiry, restoration, and concurrent updates form a tested
transaction, act at once, offer a transient, non-blocking undo control that
lasts long enough to use and is reachable by keyboard and assistive technology.
If the action later fails, restore the prior state without a blocking dialog
and say what was restored and why.
Reserve blocking confirmation for an immediate, irreversible external
consequence (sending, charging, disclosing) or where reliable recovery cannot
be built; name the object and consequence, and label the button with the
action. Test rapid repeat, cancellation, navigation, retry, optimistic
reconciliation, rollback, and stale-response races where applicable.

## Keyboard

Design keyboard paths, not just permit them. Keep focus visible and in reading
order. Preserve native keyboard behaviour in ordinary lists and tables; Tab
reaches their interactive controls. For a chosen composite widget such as a
listbox, grid, menu, or tree, follow the pattern's keyboard contract for arrow keys,
selection, activation, Home/End, and type-ahead. Escape dismisses only
the topmost layer (tooltip, then menu or popover, then dialog or sheet) and
restores focus to its invoker; unsaved input survives it as a kept draft or
undo, as it would a pointer dismissal. Where users repeatedly reach many
destinations or actions, prefer a command palette (conventional shortcut,
fuzzy search over actions and destinations, recent items, shortcuts shown in
results) to deep click-through; it supplements visible navigation, never
replaces it. Surface shortcuts where users meet them, avoid browser, platform,
and assistive-technology keys, and suspend single-key shortcuts while a text
field has focus.

Sources: W3C ARIA APG, [Table](https://www.w3.org/WAI/ARIA/apg/patterns/table/) versus
[Grid](https://www.w3.org/WAI/ARIA/apg/patterns/grid/) patterns (a static table is not a grid);
NN/g, [Confirmation dialogs can prevent user errors](https://www.nngroup.com/articles/confirmation-dialog/);
Vercel, [Web Interface Guidelines](https://vercel.com/design/guidelines).
