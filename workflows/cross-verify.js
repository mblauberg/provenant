// cross-verify — standalone cross-family verification pass over one artifact or claim.
//
// PURPOSE
//   Take any artifact (a file/report/patch) OR a single textual claim and run a
//   READ-ONLY, decorrelated cross-family verification pass over it. No source is
//   ever edited. The output is a normalised verdict report with source + exact-quote
//   anchors, so a parent workflow (or a human) can trust or drop the artifact/claim.
//   This is the SMALLEST workflow in the close-off suite and is intentionally
//   composable: research, citation, coherence, finalisation and change workflows
//   can all call it (one level of nesting) as their
//   "decorrelated review" leg, or it can be run directly.
//
// PHASE FLOW (fan-out -> reduce -> adversarial-verify -> synthesis)
//   1. Bootstrap        : one agent scaffolds the run dir (run_dir_init.sh) + reads
//                         the target into the run dir so workers share a fixed copy.
//   2. Decompose        : one mid agent splits the target into atomic checkable
//                         claims (each with where-to-check pointers). For a single
//                         {claim} arg this yields exactly one claim.
//   3. Cross-verify     : PER CLAIM, fan out BOTH a same-family Claude skeptic AND a
//                         cross-family worker (codex enforced read-only, optional
//                         cursor) that shell out to cf_dispatch.sh. Cross-family is a
//                         first-class parallel worker here, not a final gate. Each
//                         returns a normalised verdict with anchors.
//   4. Synthesis        : one flagship agent reduces per-claim verdicts into one
//                         report, reconciles disagreement (objective anchors outrank
//                         opinion; default to "unsupported" on uncertainty), and
//                         records CROSS-FAMILY-NOT-RUN reasons where the cross-family
//                         leg failed entirely. READ-ONLY: emits a report, applies
//                         nothing.
//
// MODEL-TIER ROUTING (role -> tier; never hard-code a dated model id)
//   bootstrap resolves routes; decomposition -> workhorse; skeptic -> flagship.
//   cross-family dispatch agent-> scout: drives Bash + normalises CLI JSON
//                                                     (codex -> cursor; agy advisory only).
//   final synthesis/adjudicate -> flagship: owns the call, no majority-voting
//                                                     of weak findings into truth.
//   Bootstrap resolves durable aliases through the global model router; every later
//   stage receives the concrete model for its role.
//
// WRITE-AUTHORITY / ESCALATION BOUNDARY
//   This workflow is verification-only and therefore READ-ONLY end to end. It auto-
//   applies NOTHING. It never edits the target or any source. Its single output is a
//   verdict report; any follow-on edit is the caller's separate approve-then-apply
//   step. There is consequently no serial applier here, but the verdict per claim is
//   risk-tagged so a caller can route low-risk vs high-risk downstream.

export const meta = {
  name: 'cross-verify',
  description: 'Read-only artifact/claim verification. Doctrine: the orchestration contract and Claude Workflow adapter in the orchestrate skill.',
  whenToUse: 'To get a decorrelated, source/quote-anchored verdict on an artifact or a single claim — standalone or as the cross-family leg of another workflow.',
  phases: [
    { title: 'Bootstrap' },
    { title: 'Decompose' },
    { title: 'Cross-verify' },
    { title: 'Synthesis' },
  ],
}

// ---------------------------------------------------------------------------
// Structured-output schemas (force StructuredOutput tool; validated objects back).
// ---------------------------------------------------------------------------

// Bootstrap returns the resolved run dir + the path it staged the target copy at,
// plus a runtime-resolved git cwd for codex dispatch (no hard-coded repo layout).
const BOOTSTRAP_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['runDir', 'targetPath', 'targetKind', 'targetAvailable', 'gitCwd', 'modelRoutes'],
  properties: {
    runDir: { type: 'string', description: 'Absolute path to the scaffolded run directory.' },
    targetPath: { type: 'string', description: 'Path of the staged target copy inside the run dir (findings/target.*). For a claim, the findings/target.txt the claim was written to.' },
    targetKind: { type: 'string', enum: ['artifact', 'claim'], description: 'Whether the target is an on-disk artifact or a literal claim string.' },
    targetAvailable: { type: 'boolean', description: 'true if the target exists and was staged; false if the artifact path was missing (synthesis then short-circuits to an inconclusive report).' },
    gitCwd: { type: 'string', description: 'Absolute path to a git repo dir usable as cwd for codex cross-family dispatch (cwd if it is a git repo, else the nearest enclosing/nested git dir); empty string if none was found (then cross-family falls back to cursor/agy or records CROSS-FAMILY-NOT-RUN).' },
    notes: { type: 'string', description: 'Anything surprising during staging (missing file, binary, truncation).' },
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

// Decompose returns the atomic checkable claims.
const DECOMPOSE_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['claims'],
  properties: {
    claims: {
      type: 'array',
      maxItems: 64,
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['id', 'claim', 'whereToCheck', 'riskTag'],
        properties: {
          id: { type: 'string', description: 'Stable id, e.g. C1, C2.' },
          claim: { type: 'string', description: 'A single falsifiable assertion to verify.' },
          whereToCheck: { type: 'string', description: 'Pointer(s) to the evidence locus: file path(s), citekey, route, test, or "external".' },
          riskTag: { type: 'string', enum: ['low', 'high'], description: 'low: cosmetic/internal; high: public contracts, auth/security/privacy controls, data/schema migration, persistence, release/artifact logic, generated or protected sources, ADR-governed areas, or build/test infrastructure.' },
        },
      },
    },
    notes: { type: 'string' },
  },
}

// One verifier's normalised verdict for one claim (used by both the Claude skeptic
// and the cross-family worker so synthesis reads a single shape).
const VERDICT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['claimId', 'verifier', 'verdict', 'anchors', 'crossFamily', 'readOnlyGuarantee'],
  properties: {
    claimId: { type: 'string' },
    verifier: { type: 'string', description: 'e.g. claude-skeptic, codex, cursor, CROSS-FAMILY-NOT-RUN.' },
    verdict: { type: 'string', enum: ['supported', 'unsupported', 'partial', 'unable'], description: 'Default to unsupported/unable on uncertainty; never assert support without an anchor.' },
    confidence: { type: 'number', minimum: 0, maximum: 1 },
    anchors: {
      type: 'array',
      description: 'Source + exact-quote evidence. Empty array is itself a signal (no support found).',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['source', 'quote'],
        properties: {
          source: { type: 'string', description: 'File path / citekey / route / test / URL the quote came from.' },
          quote: { type: 'string', description: 'Verbatim supporting (or refuting) text.' },
          locator: { type: 'string', description: 'Line range / section / page, if known.' },
        },
      },
    },
    crossFamily: { type: 'boolean', description: 'true only when produced by a different model family with an enforced/oauth_safe_mode read-only guarantee.' },
    readOnlyGuarantee: { type: 'string', enum: ['enforced', 'oauth_safe_mode', 'best_effort', 'none', 'unknown'], description: 'Mirrors cf_dispatch.sh read_only_guarantee for cross-family; "enforced" for the Claude skeptic.' },
    notRunReason: { type: 'string', description: 'If verdict=unable due to cross-family failure, the CROSS-FAMILY-NOT-RUN reason from the dispatcher record.' },
    reasoning: { type: 'string', description: 'One or two lines on how the anchors settle the claim.' },
  },
}

// Final report shape.
const REPORT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['reportPath', 'overall', 'perClaim', 'crossFamilyStatus'],
  properties: {
    reportPath: { type: 'string', description: 'Path to the written verdict report inside the run dir.' },
    overall: { type: 'string', enum: ['supported', 'unsupported', 'mixed', 'inconclusive'], description: 'Roll-up verdict; objective anchors outrank opinion.' },
    perClaim: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['claimId', 'consensus', 'agreement', 'riskTag'],
        properties: {
          claimId: { type: 'string' },
          consensus: { type: 'string', enum: ['supported', 'unsupported', 'partial', 'inconclusive'] },
          agreement: { type: 'string', enum: ['agree', 'split', 'single-voter'], description: 'Whether same-family and cross-family verdicts agreed.' },
          riskTag: { type: 'string', enum: ['low', 'high'], description: 'Carried through from the decomposed claim; escalation depends on it.' },
          topAnchor: { type: 'string', description: 'The single most load-bearing source+quote, if any.' },
        },
      },
    },
    crossFamilyStatus: { type: 'string', description: 'One of: "enforced" | "oauth_safe_mode" | "CROSS-FAMILY-NOT-RUN: <reason>" (agy is disabled here, so no advisory scout-only status is produced).' },
    escalations: { type: 'array', items: { type: 'string' }, description: 'High-risk or split claims a human should adjudicate (this workflow applies nothing).' },
  },
}

// ---------------------------------------------------------------------------
// Helpers — pure string builders only (script has no FS/shell/clock/RNG).
// ---------------------------------------------------------------------------

// Read the target + runId from args. Accept either {artifact} or {claim}. runId is
// supplied by the caller (the script cannot mint a timestamp); we require a usable one
// rather than defaulting to a shared dir that reruns would collide on / run_dir_init
// would refuse as non-empty.
const TARGET = (args && (args.artifact ?? args.claim)) ?? ''
const TARGET_IS_CLAIM = !!(args && args.claim != null && args.artifact == null)
const RAW_RUN_ID = (args && args.runId) || ''
// Constrain the runId to a filesystem/shell-safe slug so the unquoted-path risk below is
// closed at the source; if the caller gave nothing usable, fall back to a fixed literal
// (the bootstrap agent then re-runs run_dir_init with --force only for a stale same-id dir).
const SAFE_RUN_ID = (RAW_RUN_ID.match(/[A-Za-z0-9._-]+/g) || []).join('-')
const RUN_ID = SAFE_RUN_ID || 'cross-verify-run'
const RUN_DIR = `.work/wf/cross-verify/${RUN_ID}`
const SKILL_SCRIPTS = '~/.agents/skills/orchestrate/scripts'

function bootstrapPrompt() {
  return [
    'You are the bootstrap agent for a READ-ONLY cross-family verification run. Do FS plumbing only; make no judgements about the content.',
    '',
    `1. Run: ${SKILL_SCRIPTS}/run_dir_init.sh "${RUN_DIR}"   (the path is quoted; keep it quoted in your shell)`,
    '   It prints the resolved run-dir path on stdout. If it refuses (non-empty), re-run with --force only if the dir is clearly a stale copy of THIS runId; otherwise report the refusal in notes.',
    '   If the script is unavailable, fall back to: mkdir -p the dir plus findings/ crossfamily/ traces/ and an empty MANIFEST.md.',
    '',
    TARGET_IS_CLAIM
      ? [
          'This run verifies a literal CLAIM (not a file). Stage it so workers share one fixed copy:',
          `   Write the claim verbatim to <run-dir>/findings/target.txt:`,
          '   ---CLAIM---',
          TARGET,
          '   ---END CLAIM---',
          'Set targetKind="claim", targetPath to that file, and targetAvailable=true.',
        ].join('\n')
      : [
          'This run verifies an ARTIFACT given by path. Stage a fixed copy so all workers read the same bytes:',
          `   Source artifact path: ${TARGET}`,
          '   Copy it to <run-dir>/findings/target.<ext> (preserve extension). Always copy to the staged path — never return the original path. If it is large, copy in full anyway; if binary, record that in notes and copy as-is.',
          '   Set targetKind="artifact", targetPath to the staged copy, and targetAvailable=true.',
          '   If the path does NOT exist (or is unreadable): set targetAvailable=false, record why in notes, and set targetPath to the staged path you would have used. The workflow then short-circuits to an inconclusive report — do not invent contents.',
        ].join('\n'),
    '',
    '2. Resolve a git repo dir for codex cross-family dispatch (codex refuses untrusted non-git dirs). Report it as gitCwd:',
    '   - If your current working directory is itself inside a git repo, use that repo dir.',
    '   - Else find the nearest enclosing git repo, or a nested git dir under cwd (e.g. a sub-project).',
    '   - If no git repo exists anywhere reachable, set gitCwd="" (empty). Cross-family will then fall back to cursor/agy or record CROSS-FAMILY-NOT-RUN; there is NO --skip-git-repo-check flag.',
    '   Do not hard-code any project layout — discover this at runtime.',
    '',
    '3. Resolve concrete Claude models with `provenant route resolve` for:',
    '   flagship/lead, flagship/critical-review, workhorse/worker, scout/scout. Return resolved_model values in modelRoutes.',
    '',
    'Return ONLY the structured object. Do not paste file contents.',
  ].join('\n')
}

function decomposePrompt(boot) {
  return [
    'You split a verification target into ATOMIC, FALSIFIABLE claims for decorrelated checking. READ-ONLY: read files, write nothing to source.',
    '',
    `Run dir: ${boot.runDir}`,
    `Target (${boot.targetKind}): ${boot.targetPath}`,
    boot.notes ? `Bootstrap notes: ${boot.notes}` : '',
    '',
    TARGET_IS_CLAIM
      ? 'The target is a single claim. Usually that is exactly one claim (id C1). Only split if it bundles several independently-checkable assertions.'
      : 'Read the staged target. Extract every checkable assertion it makes about code, data, citations, results, or behaviour. Skip pure opinion/prose-style remarks.',
    '',
    'For each claim give: id (C1, C2, ...), the falsifiable assertion, whereToCheck (concrete evidence locus: file path(s)/citekey/route/test/"external"), and a riskTag.',
    'riskTag=high for public contracts, auth/security/privacy controls, data or schema migration, persistence, release/artifact logic, generated sources, ADR-governed areas, build/test infrastructure, protected project paths, or any project-declared hard gate; else low. (Tag only — this workflow edits nothing.)',
    'Cap at 64 claims; if more exist, keep the highest-signal 64 and say so in notes.',
    '',
    'Write the full claim list to <run-dir>/findings/claims.md, then return ONLY the structured object.',
  ].filter(Boolean).join('\n')
}

// Same-family Claude skeptic — decorrelated voter, prompted to refute.
function claudeSkepticPrompt(boot, claim) {
  return [
    'You are an adversarial Claude verifier. Try to REFUTE the claim. READ-ONLY: read any files/sources needed; write nothing to source.',
    '',
    `Run dir: ${boot.runDir}`,
    `Claim ${claim.id}: ${claim.claim}`,
    `Where to check: ${claim.whereToCheck}`,
    '',
    'Find the actual evidence. Quote it VERBATIM with its source path/citekey/route/test and a locator (line range/section/page).',
    'Decide a verdict: supported | unsupported | partial | unable. Default to unsupported/unable if you cannot anchor support to an exact quote — never assert support without an anchor.',
    'Set crossFamily=false and readOnlyGuarantee="enforced" (you are same-family).',
    `Write your working notes to <run-dir>/findings/${claim.id}.claude.md, then return ONLY the verdict object (claimId="${claim.id}", verifier="claude-skeptic").`,
  ].join('\n')
}

// Cross-family worker — first-class parallel worker. Shells out to cf_dispatch.sh.
// Attacks a DIFFERENT angle from the Claude skeptic (independent evidence hunt) and
// returns the dispatcher's normalised guarantees verbatim.
function crossFamilyPrompt(boot, claim, idx) {
  // Per-item variation by INDEX (no RNG): even claims lead with codex, odd lead with
  // cursor, so the two cross-family tools spread across the claim set deterministically.
  const lead = idx % 2 === 0 ? 'codex' : 'cursor'
  const second = lead === 'codex' ? 'cursor' : 'codex'
  // gitCwd is resolved at runtime by the bootstrap agent (no hard-coded repo layout).
  // Empty means no git repo was found, so codex is unusable and we fall back to cursor.
  const gitCwd = boot.gitCwd || ''
  const cwdClause = gitCwd
    ? `Set your shell cwd to the git repo at ${gitCwd} before dispatch (codex refuses untrusted non-git dirs). Lead chain: try ${lead} first, then ${second}.`
    : `No git repo was found at bootstrap, so codex is unusable here (cf_dispatch.sh exposes NO --skip-git-repo-check flag — it runs codex exec -s read-only). Lead with cursor (which does not require a git repo); only attempt codex if you can set cwd inside some nested git repo. If neither cross-family tool can run, record CROSS-FAMILY-NOT-RUN.`
  const chain = gitCwd ? `${lead} ${second}` : `cursor ${lead === 'cursor' ? second : lead}`
  return [
    'You drive a CROSS-FAMILY verifier via Bash. You do not judge the claim yourself; you run a different model family on it and normalise its answer. READ-ONLY throughout.',
    '',
    `Run dir: ${boot.runDir}`,
    `Claim ${claim.id}: ${claim.claim}`,
    `Where to check: ${claim.whereToCheck}`,
    '',
    'STEP 0 — DATA POLICY GATE (do this BEFORE writing any prompt file or dispatching). External-family CLIs disclose the prompt + any attached files to that provider. Per the cli-headless data-policy doctrine in the orchestrate skill, confirm the host project data policy permits sending THIS claim text and its cited evidence to an external provider:',
    '  - Apply the current project data policy. Redact secrets, credentials, personal data and any content not authorised for the selected provider.',
    '  - If disclosure is NOT permitted for this content: do NOT dispatch. Set verdict="unable", crossFamily=false, verifier="CROSS-FAMILY-NOT-RUN", readOnlyGuarantee="none", notRunReason="data-policy-block", and append a "CROSS-FAMILY-NOT-RUN: data-policy-block" line to <run-dir>/MANIFEST.md. Return that verdict and stop.',
    '  Record the policy acknowledgement (permitted/blocked + what, if anything, you redacted) in your norm file so it is auditable.',
    '',
    'STEP 1 — write a self-contained prompt file for the cross-family CLI:',
    `  Write to <run-dir>/crossfamily/${claim.id}.prompt.txt a prompt that asks the other model to verify the claim against the named evidence and return a verdict (supported/unsupported/partial/unable) with EXACT quotes and their source path+locator. Include the claim text and whereToCheck (these are disclosed to the external provider — STEP 0 must have cleared them).`,
    '',
    'STEP 2 — dispatch:',
    `  ${cwdClause}`,
    '  Before using cursor in the chain, run cursor-agent --list-models and export CF_DISPATCH_CURSOR_MODEL to a current model from a family distinct from Claude and OpenAI. The dispatcher fails closed if it cannot prove the provider family.',
    `  Use the --chain form so it fails over automatically:`,
    `    ${SKILL_SCRIPTS}/cf_dispatch.sh --orchestrator-family claude --chain "${chain}" \\`,
    `      --prompt-file <abs path to ${claim.id}.prompt.txt> --out <abs path to crossfamily/${claim.id}.out.txt>`,
    '  (codex runs `exec -s read-only` enforced; cursor runs `--mode plan`. Never use claude as the cross-family tool — same family. agy is advisory-only and disabled unless CF_DISPATCH_ENABLE_AGY=1; do not enable it here.)',
    '  The dispatcher prints a JSON record (adapter, provider_family, status, exit, output_path, read_only_guarantee, cross_family). Capture it.',
    '',
    'STEP 3 — normalise:',
    '  - If the record has cross_family=true and read_only_guarantee in {enforced, oauth_safe_mode} and status=ok: read the CLI answer from output_path and map it to a verdict + anchors (verbatim quotes + source + locator). Set crossFamily=true and readOnlyGuarantee accordingly. Set verifier to the tool that answered.',
    '  - If the whole chain failed (status all_failed / auth_or_quota_error / error on every tool), or no tool could run (no git repo for codex and cursor unavailable): set verdict="unable", crossFamily=false, verifier="CROSS-FAMILY-NOT-RUN", readOnlyGuarantee="none", and put the failure summary in notRunReason. Append a "CROSS-FAMILY-NOT-RUN: <reason>" line to <run-dir>/MANIFEST.md (or traces/README.md). Do NOT silently downgrade or substitute a Claude answer.',
    '',
    `Write the raw dispatcher record(s) + your normalisation to <run-dir>/crossfamily/${claim.id}.norm.md, then return ONLY the verdict object (claimId="${claim.id}").`,
  ].join('\n')
}

function synthesisPrompt(boot, decomposed, verdictRows) {
  return [
    'You are the flagship synthesiser. Reduce per-claim verdicts into one verdict report. READ-ONLY: write only the report inside the run dir; edit no source.',
    '',
    `Run dir: ${boot.runDir}`,
    `Target (${boot.targetKind}): ${boot.targetPath}`,
    `Claims checked: ${decomposed.claims.length}`,
    '',
    'You receive, per claim, a same-family Claude skeptic verdict and a cross-family verdict (or a CROSS-FAMILY-NOT-RUN marker). Note that EITHER voter may be absent if that agent died (the row is simply missing for that claimId). Reconcile what you have:',
    '  - Objective anchors (exact quotes from real sources) OUTRANK opinion. A verdict with no anchor cannot be "supported".',
    '  - If the two voters disagree, mark agreement="split" and prefer the verdict backed by the stronger anchor; if neither anchors, consensus="inconclusive".',
    '  - If only ONE voter ran (cross-family unavailable, OR the same-family Claude skeptic agent failed and left no row), agreement="single-voter". A lone unanchored voter cannot yield "supported"; default such claims to inconclusive.',
    '  - If NEITHER voter ran for a claim (both rows missing), consensus="inconclusive" and list the claim in escalations.',
    '  - carry each claim\'s riskTag through from the decomposed claim list onto its perClaim row (escalation depends on it).',
    '  - Default to unsupported/inconclusive on uncertainty. Do NOT majority-vote weak findings into truth.',
    '',
    'Roll up an overall verdict (supported/unsupported/mixed/inconclusive).',
    'Set crossFamilyStatus to the strongest guarantee actually achieved across claims ("enforced" or "oauth_safe_mode"), or "CROSS-FAMILY-NOT-RUN: <reason>" if the cross-family leg failed (or was data-policy-blocked) for every claim. Do NOT emit "scout-only" — agy is disabled in this workflow, so no advisory route exists.',
    'List escalations: every high-risk claim and every split/inconclusive/single-voter claim a human should adjudicate. This workflow applies nothing; downstream mutation is a separate approved step.',
    '',
    'Verdict rows (JSON, one or two per claim):',
    JSON.stringify(verdictRows),
    '',
    'Write the full report (overall, per-claim table with anchors, cross-family status, escalations) to <run-dir>/findings/verdict-report.md and update <run-dir>/MANIFEST.md with a row for it. Return ONLY the structured report object.',
  ].join('\n')
}

// Short-circuit report when the target could not be staged (missing artifact). No claims
// were decomposed and no verifiers ran, so we emit a single inconclusive report instead of
// hunting evidence for a file that does not exist.
function missingTargetReportPrompt(boot) {
  return [
    'You are the flagship synthesiser. The verification target could NOT be staged, so no claims were decomposed and no verifiers ran. Emit a minimal inconclusive report. READ-ONLY: write only inside the run dir.',
    '',
    `Run dir: ${boot.runDir}`,
    `Intended target (${boot.targetKind}): ${boot.targetPath}`,
    boot.notes ? `Bootstrap notes: ${boot.notes}` : 'Bootstrap reported the target as unavailable (missing or unreadable).',
    '',
    'Produce a report with: overall="inconclusive", perClaim=[] (no claims could be formed), crossFamilyStatus="CROSS-FAMILY-NOT-RUN: target-unavailable", and one escalation explaining the target path was missing/unreadable so a human can supply a valid target.',
    'Write a one-paragraph verdict-report.md to <run-dir>/findings/verdict-report.md and add a MANIFEST.md row. Return ONLY the structured report object.',
  ].join('\n')
}

// ---------------------------------------------------------------------------
// Orchestration.
// ---------------------------------------------------------------------------

// Phase 1 — Bootstrap (cheap tier: FS plumbing only).
phase('Bootstrap')
if (!TARGET) {
  log('cross-verify: no artifact/claim in args; nothing to verify. Pass {artifact|claim, runId}.')
}
const boot = TARGET
  ? await agent(bootstrapPrompt(), {
      label: 'cross-verify:bootstrap',
      phase: 'Bootstrap',
      schema: BOOTSTRAP_SCHEMA,
    })
  : null

if (boot && boot.runDir && boot.targetAvailable === false) {
  // Codex P1: artifact path was missing/unreadable. Do NOT decompose or hunt evidence on a
  // non-existent file — short-circuit straight to an inconclusive report.
  log(`cross-verify: target unavailable (${boot.targetPath}); short-circuiting to an inconclusive report.`)
  phase('Synthesis')
  const report = await agent(missingTargetReportPrompt(boot), {
    label: 'cross-verify:synthesis',
    phase: 'Synthesis',
    schema: REPORT_SCHEMA,
    model: boot.modelRoutes.flagship,
  })
  if (report) {
    log(`cross-verify: overall=${report.overall}; cross-family=${report.crossFamilyStatus}; report at ${report.reportPath}.`)
  } else {
    log('cross-verify: short-circuit synthesis returned no report; check run-dir notes for the missing-target reason.')
  }
} else if (boot && boot.runDir) {
  const models = boot.modelRoutes
  log(`cross-verify: run dir ${boot.runDir}; target ${boot.targetKind} at ${boot.targetPath}.`)

  // Phase 2 — Decompose into atomic claims (mid tier).
  phase('Decompose')
  const decomposed = await agent(decomposePrompt(boot), {
    label: 'cross-verify:decompose',
    phase: 'Decompose',
    schema: DECOMPOSE_SCHEMA,
    model: models.workhorse,
  })

  const claims = (decomposed && Array.isArray(decomposed.claims)) ? decomposed.claims : []
  if (claims.length === 0) {
    log('cross-verify: decomposition produced no checkable claims; emitting an inconclusive report.')
  } else {
    log(`cross-verify: ${claims.length} claim(s) to verify across same-family + cross-family voters.`)
  }

  // Phase 3 — Cross-verify each claim. pipeline() so each claim flows independently
  // (no barrier): per claim we fan out a Claude skeptic AND a cross-family worker in
  // parallel (both must return before this claim's verdicts are ready, hence parallel()
  // INSIDE the stage). Cross-family is a first-class parallel worker, run in lockstep
  // with the same-family skeptic — not a downstream gate.
  const verdictRows = claims.length === 0 ? [] : await pipeline(
    claims,
    (claim, _orig, idx) => parallel([
      () => agent(claudeSkepticPrompt(boot, claim), {
        label: `cross-verify:claude:${claim.id}`,
        phase: 'Cross-verify',
        schema: VERDICT_SCHEMA,
        model: models.criticalReviewer,
      }),
      () => agent(crossFamilyPrompt(boot, claim, idx), {
        label: `cross-verify:cf:${claim.id}`,
        phase: 'Cross-verify',
        schema: VERDICT_SCHEMA,
        model: models.scout,
      }),
    ]),
  )

  // Flatten + drop dead agents (parallel resolves throwers to null).
  const flatVerdicts = verdictRows.flat().filter(Boolean)
  log(`cross-verify: collected ${flatVerdicts.length} verdict(s) from ${claims.length} claim(s).`)

  // Phase 4 — Synthesis (flagship tier: adjudicate, record CROSS-FAMILY-NOT-RUN, escalate).
  // Normalise decomposed to a guaranteed {claims:[...]} shape so synthesisPrompt never reads
  // .claims off undefined (decompose may have died or returned a partial object).
  const decomposedSafe = { claims }
  phase('Synthesis')
  const report = await agent(
    synthesisPrompt(boot, decomposedSafe, flatVerdicts),
    { label: 'cross-verify:synthesis', phase: 'Synthesis', schema: REPORT_SCHEMA, model: models.flagship },
  )

  if (report) {
    log(`cross-verify: overall=${report.overall}; cross-family=${report.crossFamilyStatus}; report at ${report.reportPath}.`)
    if (report.escalations && report.escalations.length) {
      log(`cross-verify: ${report.escalations.length} item(s) escalated for human adjudication (workflow applies nothing).`)
    }
  } else {
    log('cross-verify: synthesis agent returned no report; check run-dir findings/ for partial verdicts.')
  }
}
