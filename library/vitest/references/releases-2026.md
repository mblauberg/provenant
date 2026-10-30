# Release notes

Checked: 30 October 2026. The npm registry reported Vitest 5.0.5 as the latest stable package; confirm the installed version and [supported releases](https://vitest.dev/releases). The [migration guide](https://vitest.dev/guide/migration/) documents Vitest 5 and its Node and Vite requirements; [Vite migration](https://vite.dev/guide/migration) covers Vite separately.

Vitest 5 defaults to clearing mock history between tests. Browser automocks return undefined unless configured to spy. Fake timers can fake Temporal. JSON and JUnit reporter destinations and coverage glob matching changed. Verify each relevant setting and output.
