// codebase-polish — PERSONAL, PROJECT-AGNOSTIC code-quality sweep over one repo slice.
//
// Purpose: find low-risk hygiene wins (dead code, duplication, simplification,
// lint/type nits, UI/design-token nits) AND surface SOLID/architecture smells
// (escalated by default — only isolated single-function fixes are low-risk) in a
// bounded slice, review each candidate
// from decorrelated angles (Claude + cross-family), risk-tag and adjudicate, then
// auto-apply only the low-risk edits via ONE serial applier after objective checks.
// High-risk edits are emitted as validated patches + a written recommendation for a
// separate approve-then-apply step. The workflow NEVER blocks mid-run for approval.
//
// Project-agnostic: lint/test/format conventions, any design system, and the concrete
// protected-path globs are all DISCOVERED AT RUNTIME (Makefile / AGENTS.md / package.json /
// repo layout) and returned in recon.protectedPaths — nothing project-specific is hard-coded
// in this script. The NEVER_AUTOEDIT constant carries only doctrinal CATEGORIES, not file
// names. args = { path | package, runId }.
//
// Phase flow:
//   0 Bootstrap   — one agent makes the run dir + discovers repo conventions (recon.json).
//   1 Scan        — bounded fan-out of cheap finders over the slice -> candidate list.
//   2 Review      — per candidate: parallel Claude reviewer + cross-family reviewer
//                   (cf_dispatch.sh -> codex) at a DIFFERENT angle. Cross-family is a
//                   first-class parallel worker here, not a final gate.
//   3 Adjudicate  — flagship synthesis risk-tags every candidate, splits low vs high.
//   4 Apply       — ONE serial applier lands low-risk after objective checks; high-risk
//                   stays as patches + recommendation. Final readiness report.
//
// Model-tier routing (role -> tier; per the orchestrate skill's routing doctrine, express as
// role -> tier, never a dated model ID):
//   bootstrap/recon resolves routes; finders -> scout; candidate review -> flagship;
//   cross-family driver -> scout; adjudication -> flagship; serial applier -> workhorse.
// Bootstrap resolves durable aliases through the global router; every later stage receives
// the concrete model for its role.
// Cross-family routing: cf_dispatch.sh with --orchestrator-family claude and a codex-first
// --chain (exec -s read-only, enforced). codex refuses non-git dirs and cf_dispatch.sh does
// NOT forward --skip-git-repo-check, so when recon.gitCwd is set the agent cd's into it
// before dispatch; when it is null the agent relies on the chain failing over to cursor
// (which has no git requirement). Fail-over codex -> cursor -> agy (advisory scout) is
// expressed in --chain; on total failure record CROSS-FAMILY-NOT-RUN. Per the orchestrate
// skill's cli-headless data-policy doctrine, the slice
// content is disclosed to the external provider, so the host data policy is checked first.

export const meta = {
  name: 'codebase-polish',
  description: 'Project-agnostic code-polish accelerator. Doctrine: the code-review skill and the orchestration contract plus Claude Workflow adapter in the orchestrate skill.',
  whenToUse: 'when you want a bounded code-quality cleanup of one repo slice with low-risk auto-apply and high-risk escalation',
  phases: [
    { title: 'Bootstrap' },
    { title: 'Scan' },
    { title: 'Review' },
    { title: 'Adjudicate' },
    { title: 'Apply' },
  ],
}

// ---------------------------------------------------------------------------
// Structured-output schemas (agents with `schema` are forced to emit these).
// ---------------------------------------------------------------------------

// Phase 0: run dir + discovered conventions. lintCmd/testCmd/formatCmd are EXACT
// commands the applier can shell out to; null when none was discovered.
const RECON_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['runDir', 'sliceRoot', 'isGitRepo', 'gitCwd', 'lintCmd', 'testCmd', 'formatCmd', 'designSystem', 'protectedPaths', 'notes', 'modelRoutes'],
  properties: {
    runDir: { type: 'string', description: 'Absolute path to the created run dir' },
    sliceRoot: { type: 'string', description: 'Absolute path to the slice being polished (from args.path|package)' },
    isGitRepo: { type: 'boolean', description: 'Whether sliceRoot sits inside a git repo' },
    gitCwd: { type: ['string', 'null'], description: 'A directory inside a git repo to cd into before running cf_dispatch (codex needs git); null when the slice is non-git -> codex fails closed and the chain fails over to cursor' },
    lintCmd: { type: ['string', 'null'], description: 'Exact lint command discovered from Makefile/AGENTS.md/package.json, or null' },
    testCmd: { type: ['string', 'null'], description: 'Exact narrowest test/lane command discovered, or null' },
    formatCmd: { type: ['string', 'null'], description: 'Exact format/typecheck command discovered, or null' },
    designSystem: {
      type: ['object', 'null'],
      additionalProperties: false,
      required: ['present', 'tokensPath', 'rules'],
      properties: {
        present: { type: 'boolean' },
        tokensPath: { type: ['string', 'null'], description: 'Path to design tokens/CSS, or null' },
        rules: { type: 'string', description: 'One-line summary of design-system rules to honour, or empty' },
      },
    },
    protectedPaths: { type: 'array', items: { type: 'string' }, description: 'Concrete repo globs the workflow must NEVER auto-edit, expanded from the doctrinal categories for THIS repo (generated contracts/schemas, result/figure macros, manuscript prose, bibliography/citation data, public API/route contracts, release/artifact logic, HPC/cluster paths, ADR-governed areas, build/test infra, dataset/corpus build)' },
    notes: { type: 'string', description: 'Headline recon notes for the manifest' },
    modelRoutes: {
      type: 'object', additionalProperties: false,
      required: ['flagship', 'criticalReviewer', 'workhorse', 'scout'],
      properties: {
        flagship: { type: 'string' }, criticalReviewer: { type: 'string' },
        workhorse: { type: 'string' }, scout: { type: 'string' },
      },
    },
  },
}

// Phase 1: one finder's batch of candidates over a sub-slice.
const SCAN_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['findingsPath', 'candidates'],
  properties: {
    findingsPath: { type: 'string', description: 'run-dir/findings file with full detail' },
    candidates: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['id', 'kind', 'file', 'locator', 'summary', 'proposedFix', 'finderRisk'],
        properties: {
          id: { type: 'string', description: 'Stable candidate id, e.g. "<finderId>-<n>"' },
          kind: { type: 'string', enum: ['dead-code', 'duplication', 'simplification', 'architecture', 'lint', 'type', 'design-token', 'ui-nit', 'other'] },
          file: { type: 'string', description: 'Absolute file path' },
          locator: { type: 'string', description: 'Line range or symbol the fix touches' },
          summary: { type: 'string', description: 'One-line problem statement' },
          proposedFix: { type: 'string', description: 'One-line proposed change' },
          finderRisk: { type: 'string', enum: ['low', 'high'], description: 'Finder first-cut risk guess' },
        },
      },
    },
  },
}

// Phase 2 (Claude reviewer): per-candidate verdict + authored patch when worth doing.
const REVIEW_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['id', 'angle', 'verdict', 'risk', 'rationale', 'patchPath', 'objectiveChecks'],
  properties: {
    id: { type: 'string' },
    angle: { type: 'string', description: 'The lens this reviewer used (e.g. correctness, reuse, design-token)' },
    verdict: { type: 'string', enum: ['apply', 'reject', 'needs-human'] },
    risk: { type: 'string', enum: ['low', 'high'] },
    rationale: { type: 'string', description: 'Why this verdict/risk; cite the protectedPaths rule if it forces high' },
    patchPath: { type: ['string', 'null'], description: 'run-dir/patches/<id>.diff written by this reviewer, or null if reject' },
    objectiveChecks: { type: 'array', items: { type: 'string' }, description: 'Exact commands the applier should run to gate this patch' },
  },
}

// Phase 2 (cross-family record): what cf_dispatch returned, normalised for the adjudicator.
const CF_REVIEW_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['id', 'crossFamilyRan', 'tool', 'crossFamily', 'readOnlyGuarantee', 'verdict', 'angle', 'recordPath', 'notRunReason'],
  properties: {
    id: { type: 'string' },
    crossFamilyRan: { type: 'boolean' },
    tool: { type: ['string', 'null'], description: 'codex|cursor|agy or null' },
    crossFamily: { type: 'boolean', description: 'From dispatcher JSON: cross_family' },
    readOnlyGuarantee: { type: ['string', 'null'], description: 'enforced|oauth_safe_mode|best_effort|none' },
    verdict: { type: 'string', enum: ['agree-apply', 'agree-reject', 'disagree', 'advisory', 'not-run'] },
    angle: { type: 'string', description: 'Different angle the cross-family worker attacked' },
    recordPath: { type: ['string', 'null'], description: 'crossfamily/<id>.json path' },
    notRunReason: { type: ['string', 'null'], description: 'CROSS-FAMILY-NOT-RUN reason when crossFamilyRan=false' },
  },
}

// Phase 3: adjudicated decision per candidate.
const ADJ_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['summaryPath', 'decisions'],
  properties: {
    summaryPath: { type: 'string', description: 'run-dir/SYNTHESIS.md or findings file with full reasoning' },
    decisions: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['id', 'decision', 'risk', 'patchPath', 'objectiveChecks', 'reason'],
        properties: {
          id: { type: 'string' },
          decision: { type: 'string', enum: ['auto-apply', 'escalate', 'drop'] },
          risk: { type: 'string', enum: ['low', 'high'] },
          patchPath: { type: ['string', 'null'] },
          objectiveChecks: { type: 'array', items: { type: 'string' } },
          reason: { type: 'string', description: 'Why auto-apply vs escalate vs drop, incl. cross-family agreement and protected-path rules' },
        },
      },
    },
  },
}

// Phase 4: applier result.
const APPLY_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['reportPath', 'applied', 'reverted', 'escalated', 'redAmberGreen'],
  properties: {
    reportPath: { type: 'string', description: 'run-dir report path with the full ledger' },
    applied: { type: 'array', items: { type: 'string' }, description: 'Candidate ids landed after objective checks passed' },
    reverted: { type: 'array', items: { type: 'string' }, description: 'Ids whose checks failed and were rolled back' },
    escalated: { type: 'array', items: { type: 'string' }, description: 'High-risk ids left as patches + recommendation' },
    redAmberGreen: { type: 'string', enum: ['green', 'amber', 'red'], description: 'Overall slice readiness after this pass' },
  },
}

// ---------------------------------------------------------------------------
// Shared prompt fragments.
// ---------------------------------------------------------------------------

// The worker contract (MAO): write detail to files, reply with headlines + path.
const WORKER_CONTRACT =
  'Worker contract: keep all source files READ-ONLY unless you are explicitly the serial applier. ' +
  'Write full detail to a file under the run dir; reply with ONLY headline findings, surprises, ' +
  'unresolved questions, and the file path. Do not paste full output.'

// Protected CATEGORIES that may NEVER be auto-edited regardless of discovered conventions.
// This workflow embeds conservative protected categories as project-agnostic categories ONLY — no
// project file names. The recon agent expands these categories into concrete repo globs in
// recon.protectedPaths, which is the load-bearing per-repo list passed to every later phase.
const NEVER_AUTOEDIT =
  'NEVER auto-edit (always escalate, never low-risk) — match against recon.protectedPaths plus ' +
  'these doctrinal categories: generated contracts/schemas (regen corrupts them), result/figure ' +
  'macros and any thesis/manuscript prose, bibliography and citation data, public API/route ' +
  'contracts, release/artifact logic, HPC/cluster paths, ADR-governed areas, build/marker/test ' +
  'infrastructure, and dataset/corpus build.'

// Architecture/SOLID doctrine: real SOLID fixes are usually cross-file or change
// signatures, so they default to HIGH-RISK/escalate. Only a fix contained to a single
// function with no signature/contract change and no new cross-module dependency is low-risk.
const ARCH_DOCTRINE =
  'Architecture/SOLID findings (kind=architecture) are HIGH-RISK by default and MUST escalate: ' +
  'single-responsibility violations (god functions/classes doing too much), leaky/wrong ' +
  'abstractions, tight coupling / missing dependency inversion, open-closed violations (type or ' +
  'instanceof ladders that want polymorphism), and layer/boundary breaches are normally cross-file ' +
  'or signature-changing. Tag one LOW-RISK (auto-apply-eligible) ONLY when the fix is a ' +
  'single-function internal change with NO signature/contract change and NO new cross-module ' +
  'dependency (e.g. extracting a local helper, collapsing a needless one-use wrapper, an isolated ' +
  'guard clause). Everything structural -> escalate with a patch + a written recommendation; ' +
  'never auto-apply a cross-file refactor.'

const cfDispatch =
  '~/.agents/skills/orchestrate/scripts/cf_dispatch.sh'

// ---------------------------------------------------------------------------
// Workflow body.
// ---------------------------------------------------------------------------

// args may arrive as an object (nested workflow()) OR a JSON string (top-level Workflow tool) —
// normalise both, else a stringified arg makes args.path undefined and the slice silently falls
// back to '.' (the whole repo), polishing far more than intended.
let __args = args
if (typeof __args === 'string') { try { __args = JSON.parse(__args) } catch { __args = {} } }
if (!__args || typeof __args !== 'object') __args = {}
const slice = __args.path || __args.package || '.'
const runId = __args.runId || 'codebase-polish-run'
// Loud guard: slice='.' means no path was passed (or arg delivery dropped it) and the
// recon agent will resolve it to the WHOLE repo — an expensive full-repo sweep. Surface
// it immediately rather than letting an operator discover the mis-scope in the trace.
if (slice === '.') {
  log('⚠️ slice resolved to "." — FULL-REPO sweep (expensive). Pass args.path for a sub-slice. Continuing.')
}
// run dir lives under the WORKSPACE/repo-root .work/ (transient scratch); per-doctrine the
// SCRIPT cannot touch the FS, so a bootstrap agent resolves the root and creates it. The hint
// is a RELATIVE suffix only — the agent must anchor it at the resolved root, not blindly at
// cwd (cwd may be a sub-package like fhs/, which would mis-place the run dir).
const runDirHintSuffix = `.work/wf/codebase-polish/${runId}`

// Bounded fan-out caps (≤16 concurrent agents; pilot before full sweep).
const MAX_FINDERS = 7        // Wave-A finders over the slice (incl. the architecture/SOLID angle)
const MAX_CANDIDATES = 40    // hard cap on reviewed candidates (log if we truncate)
// Each reviewed candidate spawns 2 agents (Claude + cross-family), so to keep the live
// fan-out within the ~16 concurrency ceiling we process candidates in waves of this size
// (REVIEW_WAVE * 2 <= 16). pipeline() alone would queue all candidates at once.
const REVIEW_WAVE = 8

// --- Phase 0: Bootstrap + runtime convention discovery -----------------------
phase('Bootstrap')
log(`codebase-polish: slice=${slice} runId=${runId}`)

const recon = await agent(
  [
    `You are the bootstrap + recon agent for a project-agnostic code-polish sweep of slice: ${slice}`,
    '',
    '0. FIRST resolve the workspace/repo ROOT for the run dir: prefer `git rev-parse --show-toplevel`',
    `   from the slice; if the slice is non-git, walk up for a workspace marker (AGENTS.md / CLAUDE.md / .git / package.json) and use that dir. Then anchor the run dir at <root>/${runDirHintSuffix} — do NOT create it blindly under the current cwd (cwd may be a sub-package, which mis-places .work/).`,
    `1. Create the run dir by running: ~/.agents/skills/orchestrate/scripts/run_dir_init.sh <root>/${runDirHintSuffix}`,
    '   (it prints the resolved absolute path; if the script is missing, mkdir -p the dir and its findings/ crossfamily/ traces/ patches/ subdirs and a MANIFEST.md). Also ensure a patches/ subdir exists.',
    '2. DISCOVER conventions AT RUNTIME — do not assume any project layout:',
    '   - lint/format/typecheck/test commands: read Makefile targets, AGENTS.md / CLAUDE.md validation sections, and package.json scripts. Capture the EXACT narrowest commands.',
    '   - design system: look for a design-system/ dir, design tokens, or a *-design skill. If present, capture its tokens path + one-line rules to honour.',
    '   - whether the slice is inside a git repo (for cross-family dispatch cwd). codex refuses non-git dirs and cf_dispatch does NOT forward a git-skip flag, so if a nearby git dir exists set gitCwd to it (the cf reviewer will cd there); only if NO git dir is reachable set gitCwd=null (codex then fails closed and the chain fails over to cursor).',
    '   - expand the protected-paths list with CONCRETE globs for this repo (these become recon.protectedPaths, the load-bearing per-repo protected list).',
    '3. Resolve concrete Claude models with `provenant route resolve` for flagship/lead, flagship/critical-review, workhorse/worker, and scout/scout. Return resolved_model values in modelRoutes.',
    '',
    NEVER_AUTOEDIT,
    'Record findings in the run-dir MANIFEST.md and traces/. ' + WORKER_CONTRACT,
    'Return the structured recon object.',
  ].join('\n'),
  { label: 'bootstrap+recon', phase: 'Bootstrap', schema: RECON_SCHEMA },
)

if (!recon || !recon.runDir) {
  log('Bootstrap failed (no run dir). Aborting; nothing was changed.')
  return { status: 'aborted', reason: 'bootstrap-failed' }
}
const runDir = recon.runDir
const models = recon.modelRoutes
const designLine = recon.designSystem && recon.designSystem.present
  ? `Honour the design system at ${recon.designSystem.tokensPath || '(tokens path unknown)'}: ${recon.designSystem.rules || 'use tokens, no raw values'}.`
  : 'No design system detected; skip design-token nits.'
// Concrete per-repo protected globs discovered by recon. These outrank the doctrinal
// categories and are injected into EVERY decision/apply prompt so auto-apply cannot miss a
// concrete protected path. Empty list -> rely on the doctrinal categories alone.
const protectedGlobs = (recon && Array.isArray(recon.protectedPaths)) ? recon.protectedPaths : []
const protectedLine = protectedGlobs.length
  ? `Concrete protected globs for THIS repo (NEVER auto-edit any path matching these; treat as high-risk/escalate): ${protectedGlobs.join(', ')}.`
  : 'Recon discovered no concrete protected globs; enforce the doctrinal categories above by judgement.'
log(`recon done -> ${runDir}. lint=${recon.lintCmd || 'none'} test=${recon.testCmd || 'none'} format=${recon.formatCmd || 'none'} design=${recon.designSystem && recon.designSystem.present ? 'yes' : 'no'}`)

// --- Phase 1: Scan (cheap bulk finders over sub-slices) ----------------------
// role -> tier: slice finders -> cheap (haiku). Bounded wave; each finder owns a
// distinct angle so coverage is multi-modal rather than N identical scans.
const FINDER_ANGLES = [
  { id: 'f-dead', focus: 'dead/unreachable code, unused exports, unused imports, commented-out blocks' },
  { id: 'f-dup', focus: 'duplication and copy-paste that could collapse to a shared helper' },
  { id: 'f-simplify', focus: 'over-complex expressions/control flow that simplify with no behaviour change' },
  { id: 'f-arch', focus: 'SOLID / architecture smells: single-responsibility violations (functions/classes doing too much), leaky or wrong abstractions, tight coupling / missing dependency inversion, open-closed violations (type/instanceof ladders that want polymorphism), and layering/boundary breaches. Map suspected smells with a concrete locator; do NOT attempt deep cross-file rewrites here — leave depth + risk to the reviewer. Default finderRisk:high.' },
  { id: 'f-lint', focus: 'lint/type violations surfaced by the discovered lint/type commands' },
  { id: 'f-design', focus: 'UI/design-token nits: raw colours/spacing/typography that should use design tokens' },
  { id: 'f-misc', focus: 'naming, doc/comment typos, and other isolated low-risk hygiene' },
].slice(0, MAX_FINDERS)

phase('Scan')
const scanResults = await parallel(
  FINDER_ANGLES.map((f, i) => () =>
    agent(
      [
        `Finder ${f.id} (angle ${i + 1}/${FINDER_ANGLES.length}). Scan slice ${recon.sliceRoot} for: ${f.focus}.`,
        recon.lintCmd ? `If useful, run the discovered lint command read-only: ${recon.lintCmd}` : '',
        recon.formatCmd ? `Discovered format/type command (read-only): ${recon.formatCmd}` : '',
        f.id === 'f-design' ? designLine : '',
        f.id === 'f-arch' ? ARCH_DOCTRINE : '',
        NEVER_AUTOEDIT,
        'This is a cheap-tier bulk scan: stay fast and shallow, do not over-analyse — just map candidates.',
        'Emit candidates with stable ids prefixed by your finder id. Give each a first-cut finderRisk.',
        `Write full detail to ${runDir}/findings/${f.id}.md. ` + WORKER_CONTRACT,
        'Return the structured scan object.',
      ].filter(Boolean).join('\n'),
      { label: `scan:${f.id}`, phase: 'Scan', schema: SCAN_SCHEMA, model: models.scout },
    ),
  ),
)

// Flatten candidates from every finder.
let candidates = []
for (const r of scanResults.filter(Boolean)) {
  if (r && Array.isArray(r.candidates)) candidates = candidates.concat(r.candidates)
}
// Dedupe BEFORE the cap: two finders can flag the same nit, which would otherwise produce
// conflicting duplicate patches downstream. Key on normalised file+locator+kind+proposedFix.
const seenCandidateKeys = new Set()
const dedupedCandidates = []
let duplicateCount = 0
for (const c of candidates) {
  if (!c) continue
  const norm = (v) => String(v == null ? '' : v).trim().toLowerCase().replace(/\s+/g, ' ')
  const key = [norm(c.file), norm(c.locator), norm(c.kind), norm(c.proposedFix)].join('|')
  if (seenCandidateKeys.has(key)) { duplicateCount++; continue }
  seenCandidateKeys.add(key)
  dedupedCandidates.push(c)
}
if (duplicateCount > 0) log(`Deduped ${duplicateCount} duplicate candidate(s) before review.`)
candidates = dedupedCandidates
// Cap candidates (no silent caps: log truncation).
if (candidates.length > MAX_CANDIDATES) {
  log(`Scan found ${candidates.length} candidates; reviewing first ${MAX_CANDIDATES} (capped). Remainder logged in findings.`)
  candidates = candidates.slice(0, MAX_CANDIDATES)
}
if (candidates.length === 0) {
  log('No candidates found. Slice is clean for the scanned angles.')
  return { status: 'clean', runDir, applied: [], escalated: [] }
}
log(`Scan -> ${candidates.length} candidates.`)

// --- Phases 2+3 fused per item via pipeline (no barrier between candidates) ---
// Each candidate flows: Claude review + cross-family review (parallel, different
// angles) -> nothing else here; adjudication is a single barrier AFTER, because it
// needs ALL candidate reviews together to dedupe/compare consistently.
// Each candidate spawns 2 agents, so we run the pipeline in WAVES of REVIEW_WAVE
// candidates (<= ~16 live agents) rather than queueing every candidate at once.
phase('Review')
// Stage 1 thunk for one candidate: parallel Claude reviewer + cross-family reviewer.
const reviewStage = (cand, _orig, _i) => parallel([
    // Claude reviewer (mid tier): correctness/reuse lens; authors the patch.
    () => agent(
      [
        `Claude reviewer for candidate ${cand.id} (${cand.kind}). Angle: correctness + reuse + minimal-diff + SOLID/architecture soundness.`,
        `File: ${cand.file}  Locator: ${cand.locator}`,
        `Problem: ${cand.summary}  Proposed: ${cand.proposedFix}`,
        cand.kind === 'design-token' ? designLine : '',
        cand.kind === 'architecture' ? ARCH_DOCTRINE : '',
        'Decide verdict (apply|reject|needs-human) and risk (low|high) per this rule:',
        '  low-risk = formatting, dead-code removal, comment/doc typos, import cleanup, an isolated single-function internal change with NO signature change, or a design-token nit within the design system.',
        '  high-risk = cross-file refactor, signature/API/contract change, or anything under a protected path.',
        NEVER_AUTOEDIT,
        protectedLine,
        `If worth applying, WRITE the unified diff to ${runDir}/patches/${cand.id}.diff (do NOT edit source). List the EXACT objective checks (use discovered: lint=${recon.lintCmd || 'n/a'} test=${recon.testCmd || 'n/a'} format=${recon.formatCmd || 'n/a'}) the applier must run. objectiveChecks must be NON-EMPTY for any verdict=apply (at minimum the narrowest discovered lane); an empty checks list forces this to needs-human.`,
        WORKER_CONTRACT,
        'Return the structured review object.',
      ].filter(Boolean).join('\n'),
      { label: `review:${cand.id}`, phase: 'Review', schema: REVIEW_SCHEMA, model: models.criticalReviewer },
    ),
    // Cross-family reviewer (first-class parallel worker): decorrelated angle via codex.
    // role -> tier: cross-family review -> mid (codex), enforced read-only.
    () => agent(
      [
        `Cross-family reviewer for candidate ${cand.id}. Dispatch a DIFFERENT-family model at a DIFFERENT angle (side effects, hidden callers, behaviour drift the Claude reviewer may miss).`,
        `DATA POLICY (the orchestrate skill's cli-headless data-policy doctrine): cf_dispatch discloses the slice content (${cand.file} excerpt) to an EXTERNAL provider. Before dispatching, confirm the host project's data policy permits disclosing this slice. If disclosure is NOT authorised, do NOT dispatch: set crossFamilyRan=false and notRunReason="CROSS-FAMILY-NOT-RUN: data-policy-withheld", and stop.`,
        `Write the review prompt (problem, file, locator, the proposed fix to scrutinise) to ${runDir}/crossfamily/${cand.id}.prompt.`,
        recon.gitCwd
          ? `cwd: codex (exec -s read-only) refuses non-git dirs, and cf_dispatch does NOT forward any git-skip flag — so FIRST cd "${recon.gitCwd}" (a git dir) before invoking the dispatcher, then run from there.`
          : `cwd: this slice is non-git. codex (exec -s read-only) refuses non-git dirs and cf_dispatch does NOT forward any git-skip flag, so codex will fail closed here — that is expected. The chain then fails over to cursor (no git requirement); record which tool actually ran. Do NOT shell codex directly or pass any flag the dispatcher does not parse.`,
        'Before the dispatcher, run cursor-agent --list-models and export CF_DISPATCH_CURSOR_MODEL to a current model from a family distinct from Claude and OpenAI. If none is available, let that leg fail closed and record it.',
        'Then run the dispatcher (flags only — all guidance is in this prose, not in shell comments):',
        `${cfDispatch} --orchestrator-family claude \\`,
        `  --chain "codex::low cursor:: agy:: " \\`,
        `  --prompt-file ${runDir}/crossfamily/${cand.id}.prompt \\`,
        `  --out ${runDir}/crossfamily/${cand.id}.out.txt \\`,
        `  > ${runDir}/crossfamily/${cand.id}.json`,
        'Fail-over order is codex -> cursor -> agy (agy advisory/best-effort only).',
        `The CLEAN cross-family answer is in ${runDir}/crossfamily/${cand.id}.out.txt; the normalised dispatcher record (read provider_family, cross_family and read_only_guarantee from it) is in ${runDir}/crossfamily/${cand.id}.json. Do NOT conflate the two: the .json is the record, the .out.txt is the answer.`,
        'Set recordPath to the .json path. If every tool fails (chain status all_failed) or disclosure was withheld, set crossFamilyRan=false and record CROSS-FAMILY-NOT-RUN: <reason> in the manifest. Never silently downgrade.',
        WORKER_CONTRACT,
        'Return the structured cross-family record.',
      ].join('\n'),
      { label: `cf-review:${cand.id}`, phase: 'Review', schema: CF_REVIEW_SCHEMA, model: models.scout },
    ),
  ])

// Run review in bounded waves so live fan-out stays within the concurrency ceiling.
const reviewed = []
for (let w = 0; w < candidates.length; w += REVIEW_WAVE) {
  const wave = candidates.slice(w, w + REVIEW_WAVE)
  log(`Review wave ${Math.floor(w / REVIEW_WAVE) + 1}: candidates ${w + 1}-${w + wave.length} of ${candidates.length}.`)
  const waveResults = await pipeline(wave, reviewStage)
  for (const wr of waveResults) reviewed.push(wr)
}

// Pair each candidate with its [claudeReview, cfReview] (drop dead items).
const pairs = []
for (let i = 0; i < candidates.length; i++) {
  const r = reviewed[i]
  if (!r) continue
  const [claudeReview, cfReview] = r
  if (!claudeReview && !cfReview) continue
  pairs.push({ candidate: candidates[i], claudeReview, cfReview })
}

// --- Phase 3: Adjudicate (flagship synthesis; needs ALL reviews together) -----
// Flagship synthesis/adjudication. Single agent so
// dedupe/compare is consistent; it splits auto-apply (low + objective-checkable) vs escalate.
phase('Adjudicate')
const adjudication = await agent(
  [
    `Adjudicate ${pairs.length} reviewed candidates for slice ${recon.sliceRoot}. Inputs (id, Claude verdict/risk/patchPath/checks, cross-family verdict/cross_family/read_only_guarantee) follow:`,
    JSON.stringify(
      pairs.map((p) => ({
        id: p.candidate.id,
        kind: p.candidate.kind,
        file: p.candidate.file,
        claude: p.claudeReview && {
          verdict: p.claudeReview.verdict, risk: p.claudeReview.risk,
          patchPath: p.claudeReview.patchPath, checks: p.claudeReview.objectiveChecks,
        },
        crossFamily: p.cfReview && {
          ran: p.cfReview.crossFamilyRan, tool: p.cfReview.tool, verdict: p.cfReview.verdict,
          crossFamily: p.cfReview.crossFamily, guarantee: p.cfReview.readOnlyGuarantee,
          notRun: p.cfReview.notRunReason,
        },
      })),
      null, 2,
    ),
    '',
    'For each candidate decide auto-apply | escalate | drop:',
    '  auto-apply ONLY when: risk=low AND Claude verdict=apply AND a non-null patchPath exists AND objectiveChecks is NON-EMPTY and concrete AND the file is NOT under a protected path AND (cross-family agrees OR is advisory/not-run with reason recorded — cross-family disagreement on a low-risk item demotes it to escalate). An auto-apply decision MUST carry at least one objective check; if you cannot name one, escalate instead.',
    '  escalate when: risk=high, verdict=needs-human, protected path, empty/absent objective checks, missing patch, or cross-family flags behaviour drift. Keep the patch + write a recommendation; do NOT drop it.',
    '  drop only clear rejects with no value.',
    NEVER_AUTOEDIT,
    protectedLine,
    ARCH_DOCTRINE,
    'Do not majority-vote weak findings into truth; objective checks and protected-path rules outrank opinion.',
    `Write full reasoning to ${runDir}/SYNTHESIS.md. ` + WORKER_CONTRACT,
    'Return the structured adjudication object.',
  ].join('\n'),
  { label: 'adjudicate', phase: 'Adjudicate', schema: ADJ_SCHEMA, model: models.flagship },
)

if (!adjudication || !Array.isArray(adjudication.decisions)) {
  log('Adjudication failed; emitting patches as-is for human review. Nothing auto-applied.')
  return { status: 'adjudication-failed', runDir, applied: [], escalated: pairs.map((p) => p.candidate.id) }
}

// Script-side gate: never trust the adjudicator blindly. An auto-apply decision is only
// honoured if it carries a real patchPath AND a non-empty objectiveChecks list (the schema
// permits patchPath:null / []). Anything else is force-escalated, not silently applied.
const hasGate = (d) => !!(d && d.patchPath && Array.isArray(d.objectiveChecks) && d.objectiveChecks.length > 0)
const autoApply = adjudication.decisions.filter((d) => d.decision === 'auto-apply' && hasGate(d))
const forceEscalated = adjudication.decisions.filter((d) => d.decision === 'auto-apply' && !hasGate(d))
if (forceEscalated.length > 0) {
  log(`Force-escalated ${forceEscalated.length} auto-apply decision(s) lacking a patch or objective checks.`)
}
const escalate = adjudication.decisions
  .filter((d) => d.decision === 'escalate')
  .concat(forceEscalated)
log(`Adjudicated: ${autoApply.length} auto-apply (low-risk, gated), ${escalate.length} escalate (high-risk), ${adjudication.decisions.length - autoApply.length - escalate.length} drop.`)

// --- Phase 4: Apply (ONE serial applier; objective checks gate each edit) -----
// Workhorse serial applier; exact reverse patches preserve unrelated dirty-tree changes.
// SINGLE agent = the only writer, so concurrent-write hazard on a non-git tree cannot occur.
// It applies low-risk patches one at a time, runs the objective checks, reverts on failure via
// the exact reverse patch, and leaves high-risk patches untouched as escalation output. It
// NEVER blocks for approval.
phase('Apply')
const applyResult = await agent(
  [
    'You are the SINGLE serial applier — the only agent permitted to edit source this run. Apply ONLY the auto-apply (low-risk) patches below, one at a time. NEVER apply escalate patches.',
    'Auto-apply queue:',
    JSON.stringify(autoApply.map((d) => ({ id: d.id, patchPath: d.patchPath, checks: d.objectiveChecks })), null, 2),
    'Safe per-patch protocol (apply serially — never in parallel):',
    '  1. The patch MUST apply cleanly. Dry-run first (e.g. `git apply --check <patch>`, or `patch --dry-run -p1 < <patch>` on a non-git tree). If it does NOT apply cleanly, do NOT force it — record the id as reverted/skipped with the reason and move on.',
    '  2. Before applying, record the exact file paths the patch touches (from the diff headers).',
    '  3. Apply the patch, then run its objective checks plus the slice lane below.',
    '  4. If any check fails, REVERT by applying the EXACT reverse of that one patch (`git apply -R <patch>` / `patch -R -p1 < <patch>`) — restoring ONLY the files that patch touched. NEVER `git checkout`/`git restore` whole files or run a broad reset: the worktree may already be dirty with unrelated changes you must not discard.',
    '  5. Record the id under applied or reverted accordingly.',
    `Discovered lanes — lint: ${recon.lintCmd || 'none'} | test: ${recon.testCmd || 'none'} | format/type: ${recon.formatCmd || 'none'}. Run the narrowest meaningful lane; do NOT run a broad validate-all unless the repo conventions say it is required.`,
    NEVER_AUTOEDIT,
    protectedLine,
    'Defence in depth: if any queued patch touches a protected path despite adjudication, do NOT apply it — move it to escalated with the reason.',
    'Escalation output (do NOT apply — leave the patch + write a one-paragraph recommendation with validation evidence per id):',
    JSON.stringify(escalate.map((d) => ({ id: d.id, patchPath: d.patchPath, reason: d.reason })), null, 2),
    `Write the full ledger + red/amber/green readiness + the escalation recommendations to ${runDir}/POLISH_REPORT.md and update MANIFEST.md. ` + WORKER_CONTRACT,
    'Return the structured apply object.',
  ].join('\n'),
  { label: 'serial-applier', phase: 'Apply', schema: APPLY_SCHEMA, model: models.workhorse },
)

const out = {
  status: 'awaiting-human',
  runDir,
  slice: recon.sliceRoot,
  applied: (applyResult && applyResult.applied) || [],
  reverted: (applyResult && applyResult.reverted) || [],
  escalated: (applyResult && applyResult.escalated) || escalate.map((d) => d.id),
  readiness: (applyResult && applyResult.redAmberGreen) || 'amber',
  report: (applyResult && applyResult.reportPath) || `${runDir}/POLISH_REPORT.md`,
}
log(`codebase-polish done: applied=${out.applied.length} reverted=${out.reverted.length} escalated=${out.escalated.length} readiness=${out.readiness}. Report: ${out.report}`)
return out
