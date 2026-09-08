import assert from 'node:assert/strict';
import { cp, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  checkBrowserDetector,
  renderBrowserDetector,
} from './build-browser-detector.mjs';

const runtimeRoot = path.dirname(fileURLToPath(import.meta.url));
const bundle = path.join(runtimeRoot, 'detector', 'detect-antipatterns-browser.js');

test('browser detector rebuild is deterministic and detects shared-rule drift', async t => {
  const rendered = await renderBrowserDetector({ runtimeRoot });
  const checkedIn = await readFile(bundle, 'utf8');

  assert.equal(rendered, checkedIn);
  assert.equal(await renderBrowserDetector({ runtimeRoot }), rendered);
  for (const api of [
    'impeccableDetect',
    'impeccableDetectAsync',
    'impeccableScan',
    'impeccableScanAsync',
    'impeccableCollectVisualContrastCandidates',
    'impeccableAnalyzeVisualContrast',
    'impeccableGetLastVisualContrastAnalyses',
  ]) {
    assert.match(rendered, new RegExp(`window\\.${api}\\s=`));
  }

  const fixture = await mkdtemp(path.join(os.tmpdir(), 'provenant-browser-detector-'));
  t.after(() => rm(fixture, { recursive: true, force: true }));
  await cp(path.join(runtimeRoot, 'detector'), path.join(fixture, 'detector'), { recursive: true });
  const constants = path.join(fixture, 'detector', 'shared', 'constants.mjs');
  await writeFile(
    constants,
    (await readFile(constants, 'utf8')).replace("'blockquote'", "'drift-marker'"),
  );

  await assert.rejects(
    checkBrowserDetector({ runtimeRoot: fixture }),
    /browser detector is out of date/,
  );
  assert.equal(
    await readFile(path.join(fixture, 'detector', 'detect-antipatterns-browser.js'), 'utf8'),
    checkedIn,
  );

  const checks = path.join(fixture, 'detector', 'rules', 'checks.mjs');
  await writeFile(
    checks,
    (await readFile(checks, 'utf8')).replace("../shared/color.mjs", 'unexpected-package'),
  );
  await assert.rejects(
    renderBrowserDetector({ runtimeRoot: fixture }),
    /imports must match the supported local browser dependencies/,
  );
});
