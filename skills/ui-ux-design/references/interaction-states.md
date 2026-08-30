<!-- Modified for Provenant. -->

# Interaction and states

Model the user's goal as a compact state matrix: state, trigger/event, guard,
feedback, entry/exit, success, failure, recovery, and interruption. Make
impossible transitions explicit. Require every applicable state, not the same
fixed list for every element. Consider default, hover, focus, active, selected,
disabled, loading, empty, partial, success, error, permission, offline,
conflict, and stale data only where the control or flow can reach them.

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

Offer undo only when retention, expiry, restoration, concurrent updates and failure handling
form a tested transaction. Otherwise use proportionate
confirmation for irreversible or high-cost actions. Test rapid repeat,
cancellation, navigation, retry, optimistic reconciliation, rollback, and
stale-response races where applicable.
<!-- Modified for Provenant. -->
