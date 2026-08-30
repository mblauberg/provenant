<!-- Modified for Provenant. -->

# Motion

Before adding motion, name its purpose: feedback, continuity, spatial
orientation, state legibility, explanation, or rare delight. Consider how often
the user sees it; high-frequency and information-dense actions usually need
restraint or an immediate static response.

High-frequency or keyboard-driven actions should usually snap or settle
quickly. Nearby tooltips may appear immediately after the first reveal; overlays
should originate near their trigger when that preserves spatial meaning.
Retarget an in-flight transition from its current state instead of restarting,
and use stagger only when infrequent, non-blocking, and meaningful.

Choose the simplest mechanism that remains interruptible and correct. Preserve
input responsiveness, spatial continuity, exit/re-entry behaviour, and final
state under rapid interaction. Animate layout only when the chosen mechanism is
appropriate and measured on target devices. There is no universal duration,
easing, spring, property ban, or decorative recipe.

Test CPU/GPU cost, layout and paint effects, loading contention, dropped
frames, interruption, navigation, and repeated triggers in the actual surface.
Slow or step through consequential sequences to expose discontinuity, clipped
layers, incorrect origin, and timing dependencies; the slowed view is a
diagnostic, not the shipped speed.

`prefers-reduced-motion` needs an equivalent cue and usable state transition,
not just shorter animation. Remove non-essential motion, avoid parallax or
continuous movement where it impairs use, and keep focus and content changes
legible without animation. Field performance claims require field evidence.
