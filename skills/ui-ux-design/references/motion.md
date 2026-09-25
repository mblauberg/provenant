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
- **Exits** are usually a little shorter than entries and never block the next
  input; set timing by distance, velocity, and context.
- **Minimum perceptible change:** travel of only a few pixels, or a run under
  about 100 ms, reads as flicker. Give it enough distance and time to be seen,
  or use an opacity or instant state change instead.

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
not frame-by-frame analysis. Cover both the normal and the reduced-motion path.

1. Assert the mechanism for its motion type with the browser's own events,
   animation APIs, and performance evidence, including layout and paint cost on
   the target surface:
   - **Enter/exit:** completion (a CSS end event, Web Animations `finished`, or
     the spring's completion callback) reaches the intended final state, and
     nothing moves after settle.
   - **Continuous or loading:** runs only while its condition holds and stops
     or hands off when it clears; there is no end event to wait for.
   - **Interrupted or cancelled:** a mid-flight trigger retargets from the
     current state, and cancellation (such as `transitioncancel`) leaves a
     valid state rather than snapping to a stale end.
   - **Instant reduced-motion substitute:** check the final state and focus;
     it has no end event.
2. Record each path once at shipped speed with the trigger and the settled
   state visible.
3. For enter/exit motion, add a timestamped four-frame strip (entry,
   mid-transition, settle, exit) and note the exit's start and end times.
4. Slow or step through playback only when the recording or strip shows a
   problem, to isolate continuity, origin, clipping, or retargeting under
   interruption and repeated triggers. The slowed view is a diagnostic, not
   the shipped speed.
5. Record the viewport, device class, and whether reduced motion was on for
   each capture.
6. Label each artifact `verified` or `judgement`. A frozen or skipped animation
   is not evidence of motion quality, and a clean assertion does not replace
   inspecting the recording or its frames.

Sources: Apple, [Animate with springs](https://developer.apple.com/videos/play/wwdc2023/10158/)
(WWDC23); Emil Kowalski, [Great animations](https://emilkowal.ski/ui/great-animations);
Rauno Freiberg, [Invisible details of interaction design](https://rauno.me/craft/interaction-design).
