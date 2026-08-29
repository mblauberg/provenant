<!-- Modified from Impeccable for this harness; see the repository THIRD_PARTY_NOTICES.md. -->

# Responsive accessibility

Choose representative widths from content and supported devices, not a fixed
device list. Exercise narrow, intermediate, and wide layouts plus relevant
pointer, touch, keyboard, and assistive-technology paths. Adapt structure and
priority rather than merely shrinking dimensions.

Check zoom and text-only zoom, reflow, enlarged and translated content, RTL
where supported, safe areas, orientation, virtual keyboards, responsive media,
and long unbroken values. Verify that controls remain reachable, labels and
errors stay associated, focus order matches reading order, and touch targets
remain usable in their real spacing context. A 16px input font is a common way
to avoid mobile form zoom, not a universal typography minimum and not a touch
target dimension.

Transform navigation and content priority when width or input changes demand
it. Use `srcset`/`sizes` for responsive resolution and `picture` when art
direction changes. For localisation, test CJK and emoji, expansion, plural
rules, locale-aware dates/numbers/currency, and logical properties in supported
RTL layouts. Apply print or email adaptations only when those outputs are in
scope; record each as tested, not tested, or not applicable.

Test actual colour pairs, non-colour status cues, visible focus, semantic
structure, accessible names, keyboard operation, forced colours, reduced
motion, and equivalent alternatives. Route current WCAG and platform-version
claims to `web-stack-conventions`; this method does not certify full WCAG
conformance or assistive-technology coverage.

Emulation is useful evidence but does not reproduce every browser chrome,
device pixel, input, performance, safe-area, or accessibility setting. Record
which real devices and technologies were tested, failed, not tested, or not
applicable.
