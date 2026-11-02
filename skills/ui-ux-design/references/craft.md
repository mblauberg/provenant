# UI craft and evidence

Map the changed flow's states against the [design defaults](principles.md#states-every-surface-defines), including recovery where reachable. Give controls semantic names, visible focus and keyboard operation; verify focus entry, containment, dismissal and restoration for the widget pattern in use: dialogs and sheets take focus, while tooltips and combobox popups keep it on the trigger or input. Test composite widgets (grids, menus, listboxes, tabs) against their WAI-ARIA Authoring Practices keyboard model.

Choose representative widths and content stress cases from the product's supported range. Check clipping, reflow, long text, enlarged text and supported themes. Inspect actual contrast, non-colour status cues, reduced motion and accessible names where relevant. Confirm critical interactions with keyboard and pointer. Observe motion over time as well as in still captures.

Record each selected surface, state, viewport and input mode with its evidence and status: tested, failed, not tested or not applicable. Distinguish observed facts from judgement and identify the user impact and smallest useful remedy. A screenshot cannot prove assistive-technology support, full WCAG conformance, field performance or cross-browser coverage.
