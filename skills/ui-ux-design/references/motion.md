<!-- Modified for Provenant. -->

# Motion

Before adding motion, name its purpose: feedback, continuity, spatial
orientation, state legibility, explanation, or delight. Delight is a legitimate
choice when scaled to frequency. Consider how often the user sees it; the more
frequent or information-dense the action, the subtler the motion, down to an
immediate static response.

High-frequency or keyboard-driven actions should usually snap or settle
quickly. Nearby tooltips may appear immediately after the first reveal; overlays
should originate near their trigger when that preserves spatial meaning.
Retarget an in-flight transition from its current state instead of restarting,
and use stagger only when infrequent, non-blocking, and meaningful.

## Baselines

Start from these ranges and override them with project tokens and measurement
on the target surface; the numbers are starting points, not universal law.

- **Movement** (drawers, sheets, popovers, drag, selection): springs, which keep
  velocity when interrupted. Start near 0.25-0.45 s perceived duration with
  bounce 0-0.15; reserve noticeable bounce (around 0.3) for rare, playful
  moments.
- **Fades and colour changes:** easing curves on opacity and colour, usually
  ease-out, about 100-200 ms for feedback and 150-300 ms for surfaces.
- **Exits** run faster than entries, roughly two-thirds of the entry, and never
  block the next input.
- **Minimum perceptible change:** travel of only a few pixels, or a run under
  about 100 ms, reads as flicker. Give it enough distance and time to be seen,
  or use an opacity or instant state change instead.
- **Interruption:** every movement retargets from its current position and
  velocity; a new trigger does not wait for the old animation or jump to its end.
- **Frequency:** repeated, dense actions get the low end of each range or none;
  rare moments may use the high end.

Choose the simplest mechanism that remains interruptible and correct. Preserve
input responsiveness, spatial continuity, exit/re-entry behaviour, and final
state under rapid interaction. Prefer compositor-friendly properties; animate
layout only when the mechanism suits it and it is measured on target devices.
Test CPU/GPU cost, layout and paint effects, loading contention, dropped
frames, interruption, navigation, and repeated triggers in the actual surface.

`prefers-reduced-motion` needs a substitute that conveys the same state change,
not the same animation played faster: a short fade or an instant change with a
clear end state. Remove non-essential motion, avoid parallax or continuous
movement where it impairs use, and keep focus and content changes legible
without animation. Field performance claims require field evidence.

## Motion evidence

Run this for every change that adds or alters motion; it is a small fixed set,
not frame-by-frame analysis.

1. Assert completion through the mechanism in use: CSS end events, Web
   Animations completion, or a spring's completion callback. Verify the intended
   final state and no unintended motion after settle; an instant reduced-motion
   substitute needs no end event. Use browser performance evidence to check
   layout and paint cost on the target surface.
2. Record once at shipped speed with the trigger and the settled state visible.
3. Add a timestamped four-frame strip: entry, mid-transition, settle, and exit.
4. Slow or step through playback only when the recording or strip shows a
   problem, to isolate continuity, origin, clipping, or retargeting under
   interruption and repeated triggers. The slowed view is a diagnostic, not
   the shipped speed.
5. Record the viewport, device class, and whether reduced motion was on.
6. Label the recording and strip `verified` or `judgement`. A frozen or skipped
   animation is not evidence of motion quality, and a clean assertion does not
   replace inspecting the recording or its frames.

Sources: Apple, [Animate with springs](https://developer.apple.com/videos/play/wwdc2023/10158/)
(WWDC23); Emil Kowalski, [Great animations](https://emilkowal.ski/ui/great-animations);
Rauno Freiberg, [Invisible details of interaction design](https://rauno.me/craft/interaction-design).
