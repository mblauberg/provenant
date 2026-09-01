// /implement-run — Claude accelerator for the global implement skill.
//
// PERSONAL, PROJECT-AGNOSTIC implementation workflow. Discovers the
// host project's conventions (build/lint/test/format/design-system) at runtime — it hard-codes
// nothing project-specific. Driven by a Claude orchestrator. Codex is the
// load-bearing other primary; Cursor/xAI and Agy/Gemini are opportunistic,
// non-blocking workers and advisory reviewers.
//
// Phase flow (fan-out -> reduce -> adversarial-verify -> synthesis, per the orchestrate skill's orchestration contract):
//   0. bootstrap     -> one agent scaffolds the run dir (run_dir_init.sh) + sniffs conventions.
//   1. understand    -> parallel readers (Claude) + a cross-family explorer at a different angle.
//   2. plan          -> independent plan framings (minimal-diff / robust-refactor / UX-first),
//                       each weighed, one chosen by the flagship orchestrator.
//   3. implement     -> a builder agent emits PATCHES to patches/ (never edits source directly).
//   4. review        -> independent code-review lenses + other-family reviewer.
//   5. verify        -> objective checks mapped to acceptance criteria.
//   6. repair        -> risk-tier budget: substantial 4, crucial/terminal 5 cycles.
//   7. apply-gate    -> a SINGLE SERIAL applier auto-applies LOW-risk patches after checks pass,
//                       guarding on a clean target worktree + `git apply --check` before each apply;
//                       HIGH-risk patches (and any that drift / fail --check) are left validated +
//                       with a written recommendation for a separate approve-then-apply step. The run
//   8. human-gate    -> machine work ends awaiting explicit human acceptance.
//
// Concrete models are resolved once at bootstrap from durable aliases; later stages receive them.
//   bulk read / convention sniff / grep-map      -> cheap tier (intent)
//   plan framing / per-angle review / build       -> mid tier (intent)
//   plan adjudication / synthesis / apply-gate     -> flagship tier (intent; the orchestrator session)
//   other-primary explore + review                 -> Codex via cf_dispatch.sh
//   distinct-family independent lens               -> Cursor/xAI or Agy/Gemini when warranted

export const meta = {
  name: 'implement-run',
  description: 'Claude accelerator for the installed implement skill: approved contract, routed implementation, independent review, bounded repair, serial apply/escalation, human gate.',
  whenToUse: 'an approved ordinary software change with acceptance criteria; unresolved requirements route to scope',
  phases: [
    { title: 'Bootstrap' },
    { title: 'Understand' },
    { title: 'Plan' },
    { title: 'Implement' },
    { title: 'Review' },
    { title: 'Verify' },
    { title: 'Repair' },
    { title: 'Apply' },
    { title: 'Human gate' },
  ],
}

const REPAIR_BUDGETS = Object.freeze({
  routine: 2,
  substantial: 4,
  crucial: 5,
  terminal: 5,
})

// ---------------------------------------------------------------------------
// Structured-output schemas (each agent that returns data is forced through one).
// ---------------------------------------------------------------------------

// Bootstrap: resolved run dir + sniffed project conventions.
const BOOTSTRAP_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['runDir', 'repoRoot', 'gitCwd', 'baseRevision', 'effectiveRisk', 'riskPreflightPassed', 'conventions', 'modelRoutes'],
  properties: {
    runDir: { type: 'string', description: 'Absolute path returned by run_dir_init.sh' },
    repoRoot: { type: 'string', description: 'Absolute path to the project root the task targets' },
    gitCwd: {
      type: 'string',
      description: 'Absolute path to a git repo dir usable as cwd for codex dispatch; empty string if none (cf_dispatch.sh has NO --skip-git-repo-check flag, so codex cross-family then falls back to a nested git repo, else cursor/agy, else CROSS-FAMILY-NOT-RUN)',
    },
    baseRevision: { type: 'string', description: 'Git HEAD captured before any source mutation' },
    effectiveRisk: { type: 'string', enum: ['substantial', 'crucial', 'terminal'] },
    riskPreflightPassed: { type: 'boolean' },
    conventions: {
      type: 'object',
      additionalProperties: false,
      required: ['testLane', 'lintCmd', 'formatCmd', 'designSystem', 'notes'],
      properties: {
        testLane: { type: 'string', description: 'Narrowest meaningful test command discovered (Makefile/package.json/AGENTS.md); empty if none found' },
        lintCmd: { type: 'string', description: 'Lint/type command discovered; empty if none' },
        formatCmd: { type: 'string', description: 'Formatter command discovered; empty if none' },
        designSystem: { type: 'string', description: 'Design-system entry point/skill to honour for UI work; empty if none' },
        notes: { type: 'string', description: 'AGENTS.md / CLAUDE.md rules that constrain edits (e.g. never auto-edit certain files)' },
      },
    },
    modelRoutes: {
      type: 'object',
      additionalProperties: false,
      required: ['flagship', 'criticalReviewer', 'workhorse', 'scout'],
      properties: {
        flagship: { type: 'string' },
        criticalReviewer: { type: 'string' },
        workhorse: { type: 'string' },
        scout: { type: 'string' },
      },
    },
  },
}

// Understand: a reader's headline findings + the file it wrote.
const UNDERSTAND_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['angle', 'findings', 'touchedFiles', 'risks', 'path'],
  properties: {
    angle: { type: 'string', description: 'The lens this reader took (e.g. data-flow, callers, tests, UI)' },
    findings: { type: 'array', items: { type: 'string' }, maxItems: 6 },
    touchedFiles: { type: 'array', items: { type: 'string' }, description: 'Files the change is likely to touch' },
    risks: { type: 'array', items: { type: 'string' } },
    path: { type: 'string', description: 'Run-dir findings file with full notes' },
  },
}

// Plan: one independent framing of how to do the task.
const PLAN_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['framing', 'steps', 'filesToEdit', 'tradeoffs', 'riskTier', 'confidence'],
  properties: {
    framing: { type: 'string', description: 'minimal-diff | robust-refactor | ux-first | other (named)' },
    steps: { type: 'array', items: { type: 'string' } },
    filesToEdit: { type: 'array', items: { type: 'string' } },
    tradeoffs: { type: 'string', description: 'What this framing wins and loses vs alternatives' },
    riskTier: { type: 'string', enum: ['low', 'high'], description: 'Predicted escalation tier of the resulting edits' },
    confidence: { type: 'number', minimum: 0, maximum: 1 },
  },
}

// Plan adjudication: the chosen framing + why.
const CHOSEN_PLAN_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['chosenFraming', 'rationale', 'steps', 'filesToEdit', 'riskTier', 'rejected'],
  properties: {
    chosenFraming: { type: 'string' },
    rationale: { type: 'string', description: 'Why this framing beat the others (weighed, not majority-voted)' },
    steps: { type: 'array', items: { type: 'string' } },
    filesToEdit: { type: 'array', items: { type: 'string' } },
    riskTier: { type: 'string', enum: ['low', 'high'] },
    rejected: { type: 'array', items: { type: 'string' }, description: 'Framings set aside + one-line reason each' },
  },
}

// Implement: patches emitted to patches/, each self-tagged.
const IMPLEMENT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['patches', 'summary', 'path'],
  properties: {
    patches: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['patchPath', 'targetFiles', 'riskTier', 'reason'],
        properties: {
          patchPath: { type: 'string', description: 'Run-dir patches/ file (unified diff)' },
          targetFiles: { type: 'array', items: { type: 'string' } },
          riskTier: { type: 'string', enum: ['low', 'high'] },
          reason: { type: 'string', description: 'Why this tier; cites the rule that forces high-risk if so' },
        },
      },
    },
    summary: { type: 'string' },
    path: { type: 'string', description: 'Run-dir findings file with the build narrative' },
  },
}

// Review (Claude and cross-family share this shape): a verdict per angle.
const REVIEW_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['angle', 'verdict', 'issues', 'crossFamily', 'path'],
  properties: {
    angle: { type: 'string', description: 'correctness | regression | scope | cross-family' },
    verdict: { type: 'string', enum: ['approve', 'approve-with-nits', 'block'] },
    issues: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['severity', 'patchPath', 'detail'],
        properties: {
          severity: { type: 'string', enum: ['P0', 'P1', 'P2'] },
          patchPath: { type: 'string' },
          detail: { type: 'string' },
        },
      },
    },
    crossFamily: {
      type: 'object',
      additionalProperties: false,
      required: ['ran', 'tool', 'status', 'modelFamily', 'endpointProvider', 'crossFamily', 'certificationEligible', 'readOnlyGuarantee', 'outputPath', 'routeReceipt', 'notRunReason'],
      properties: {
        ran: { type: 'boolean' },
        tool: { type: 'string', description: 'codex | cursor | agy | "" if Claude reviewer' },
        status: { type: 'string', description: 'Normalised dispatcher status; not-applicable for native Claude' },
        modelFamily: { type: 'string', description: 'Actual model lineage from the dispatcher' },
        endpointProvider: { type: 'string' },
        crossFamily: { type: 'boolean' },
        certificationEligible: { type: 'boolean' },
        readOnlyGuarantee: { type: 'string', description: 'cf_dispatch.sh value: enforced | oauth_safe_mode | best_effort | prompt_only | none (none only for a Claude reviewer)' },
        outputPath: { type: 'string' },
        routeReceipt: { type: 'string', description: 'Exact cf_dispatch JSON receipt path; empty for native review' },
        notRunReason: { type: 'string', description: 'CROSS-FAMILY-NOT-RUN reason if ran=false; else ""' },
      },
    },
    path: { type: 'string' },
  },
}

// Verify: objective-check outcome on the narrowest lane.
const VERIFY_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['command', 'passed', 'summary', 'path'],
  properties: {
    command: { type: 'string', description: 'Exact command run (or "" + reason if no lane exists)' },
    passed: { type: 'boolean' },
    summary: { type: 'string' },
    path: { type: 'string' },
  },
}

const COUNCIL_CHALLENGE_SCHEMA = {
  type: 'object', additionalProperties: false,
  required: ['anonymized', 'randomized', 'path'],
  properties: {
    anonymized: { type: 'boolean' }, randomized: { type: 'boolean' }, path: { type: 'string' },
  },
}

const COUNCIL_REDUCTION_SCHEMA = {
  type: 'object', additionalProperties: false,
  required: ['freshContext', 'unresolvedDissent', 'path'],
  properties: {
    freshContext: { type: 'boolean' },
    unresolvedDissent: { type: 'array', items: { type: 'string' } },
    path: { type: 'string' },
  },
}

// Apply gate: what the serial applier landed vs escalated.
const APPLY_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['applied', 'escalated', 'manifestPath', 'recommendationPath', 'runPath', 'machineGatePassed'],
  properties: {
    applied: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['patchPath', 'targetFiles'],
        properties: {
          patchPath: { type: 'string' },
          targetFiles: { type: 'array', items: { type: 'string' } },
        },
      },
    },
    escalated: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['patchPath', 'reason'],
        properties: {
          patchPath: { type: 'string' },
          reason: { type: 'string', description: 'Why high-risk; the approval gate it needs' },
        },
      },
    },
    manifestPath: { type: 'string', description: 'Run-dir MANIFEST.md' },
    recommendationPath: { type: 'string', description: 'Run-dir file with the human approve-then-apply recommendation' },
    runPath: { type: 'string', description: 'Run-dir RUN.json lifecycle receipt' },
    machineGatePassed: { type: 'boolean' },
  },
}

// ---------------------------------------------------------------------------
// Shared prose: the worker contract + the escalation rules every agent must obey.
// ---------------------------------------------------------------------------

const WORKER_CONTRACT =
  'Worker contract: keep ALL source files read-only — you have no assigned edit scope. Write full ' +
  'output to a file under the run dir (findings/ or patches/ as instructed) and reply ONLY with your ' +
  'structured result: headline findings, surprises, unresolved questions, and the file path. Do not ' +
  'paste full output. Preserve claim / source / confidence / unresolved / prohibited-action / ' +
  'validation across handoffs.'

// Project-agnostic risk rules. The absolute-prohibition list is sourced FIRST from the host repo's
// declared never-edit files (bootstrap puts them in conventions.notes); the named categories below are
// generic defaults that degrade gracefully — they simply never fire on a repo that lacks them.
const ESCALATION_RULES =
  'Risk tagging (the proposing agent tags every edit; a later stage adjudicates):\n' +
  '- LOW-RISK (eligible for auto-apply after objective checks): formatting, dead-code removal, ' +
  'comment/docstring/doc typos, import cleanup, an isolated single-function internal change with NO ' +
  'signature change, design-token nits within the project design system.\n' +
  '- HIGH-RISK (NEVER auto-apply; emit a validated patch + written rationale for a separate approve step): ' +
  'cross-file refactors; public API / contract / route changes (and any generated-contract regen); ' +
  'release/artifact logic; HPC/cluster paths; ADR-governed areas; Makefile/markers/test infrastructure; ' +
  'dataset/corpus build.\n' +
  'ABSOLUTE PROHIBITION — always high-risk, never auto-edit under any circumstance: (1) ANY file the ' +
  'host repo declares off-limits in its conventions notes (AGENTS.md / CLAUDE.md rules passed to you), ' +
  'and (2) any bibliography, generated contract, release config, dataset, or ADR. If the host is a ' +
  'thesis repo, this includes thesis prose, references.bib, the citation matrix, and result macros. ' +
  'If the task targets any of these, emit a recommendation only.'

// Cross-family dispatch instructions reused by explorer + reviewer agents. codex refuses untrusted
// (non-git) dirs, so the dispatch runs with cwd inside a git repo; cf_dispatch.sh has NO
// --skip-git-repo-check flag. The "codex cursor" --chain lets the dispatcher own failover in one call
// (codex enforced -> cursor enforced), so cursor is attempted deterministically, not on agent whim.
function crossFamilyDispatchHint(runDir, gitCwd, kind = 'primary') {
  const cwdClause = gitCwd
    ? `Run the dispatcher with your shell cwd set to the git repo at ${gitCwd} (codex refuses non-git trees).`
    : 'No git repo was found at bootstrap. Set the shell cwd inside a nested git repo before the Codex dispatch; if none exists, record OTHER-PRIMARY-NOT-RUN.'
  if (kind === 'primary' || kind === 'primary-review') return (
    'Dispatch the load-bearing OpenAI other-primary worker. ' +
    cwdClause +
    ' Write your prompt to a file, then run:\n' +
    '  "$(provenant root)/skills/orchestrate/scripts/cf_dispatch.sh" ' +
    '--orchestrator-family anthropic --tool codex --alias flagship --role other-primary --prompt-file <your-prompt-file> ' +
    `--out "${runDir}/crossfamily/<name>.txt" > "${runDir}/crossfamily/<name>.route.json"\n` +
    (kind === 'primary-review'
      ? 'For this certifying review, the provider output at <your --out path> is the retained terminal result: ask the provider to return exactly one JSON object matching the existing REVIEW_SCHEMA keys angle, verdict, issues, crossFamily and path, with no markdown or prose. If the provider has no verdict, preserve that failure as non-certifying; never manufacture JSON in the wrapper. '
      : '') +
    'The dispatcher prints a normalised JSON record (model_family, endpoint_provider, output_digest, cross_family, certification_eligible, read_only_guarantee, ' +
    'status) — preserve that exact route JSON and return its path as routeReceipt. Certified only when cross_family=true and read_only_guarantee ' +
    'is enforced or oauth_safe_mode. On failure, set ran=false and record OTHER-PRIMARY-NOT-RUN: <reason>. ' +
    'Apply the host data policy before dispatch.'
  )
  return (
    'Use a distinct-family reviewer when warranted. Prefer a current xAI model through cursor-agent; ' +
    'Gemini through Agy is also valid. Discover and pin the exact model, keep the route read-only or ' +
    'best-effort, and capture adapter, model_family, status and output. Do not substitute Claude or ' +
    'OpenAI and do not wait or retry on quota/API failure. Any skipped leg is ' +
    'DISTINCT-FAMILY-NOT-RUN: <reason> and never replaces the other-primary gate. Distinct-family ' +
    'findings are advisory until a primary-family reviewer corroborates their evidence.'
  )
}

// ---------------------------------------------------------------------------
// Workflow body.
// ---------------------------------------------------------------------------

const task = (args && args.task) || ''
const riskHint = (args && args.risk) || 'unspecified'
const normalisedRisk = String(riskHint).toLowerCase()
const requiresOtherPrimary = true
const receiptRisk = ['crucial', 'terminal'].includes(normalisedRisk)
  ? normalisedRisk
  : 'substantial'
const specApproved = !!(args && args.specApproved)
const designStatus = (args && args.designStatus) || ''
const acceptanceCriteria = (args && args.acceptanceCriteria) || []
// runId comes from args; the script has no clock (no Date.now), so it cannot mint a unique id itself.
// run_dir_init.sh refuses a non-empty dir, so a fixed default would collide on rerun — require it,
// or let the bootstrap agent derive a unique run dir (it has a shell + clock).
const runId = (args && args.runId) || ''

if (!task) {
  log('No args.task supplied — nothing to change. Pass { task, runId, risk? }.')
  return
}
if (!specApproved || !['approved', 'not-required'].includes(designStatus) || acceptanceCriteria.length === 0) {
  log(
    'Entry gate failed. Pass specApproved=true, designStatus=approved|not-required, and at least one ' +
      'acceptanceCriteria item. Route unresolved scope/design to the global scope skill; no edits made.',
  )
  return
}
if (!runId) {
  log(
    'No args.runId supplied. The script has no clock and run_dir_init.sh refuses non-empty dirs, so ' +
      'reusing a fixed id would collide across runs. The bootstrap agent will derive a unique run dir below.',
  )
}

// --- Phase 0: bootstrap (one agent: scaffold run dir + sniff conventions). cheap-tier intent; inherits session model. ---
phase('Bootstrap')
const runIdClause = runId
  ? `Use run id "${runId}".`
  : 'No run id was supplied: derive a UNIQUE one yourself (you have a shell + clock, the script does ' +
    'not), e.g. implement-$(date +%Y%m%d-%H%M%S), so the run dir is empty for run_dir_init.sh.'
const boot = await agent(
  'Bootstrap a dynamic-workflow run.\n' +
    `1. Resolve the WORKSPACE ROOT (the dir that holds .work/, or the outermost project dir if none) and ` +
    `build an ABSOLUTE run-dir path <workspace-root>/.work/wf/implement/<runId> so the run dir never lands ` +
    `under a nested subproject. ${runIdClause}\n` +
    '   Then run: "$(provenant root)/skills/orchestrate/scripts/run_dir_init.sh" "<abs run-dir>"\n' +
    '   and ALSO run: mkdir -p "<abs run-dir>/patches"   (the patch-emitting builder writes there; ' +
    'run_dir_init.sh scaffolds findings/ crossfamily/ traces/ but NOT patches/).\n' +
    '   If run_dir_init.sh is unavailable or fails, return no runDir and stop; do not create an incomplete fallback.\n' +
    `   Copy the global deliver RUN.template.json to <abs run-dir>/RUN.json immediately. Set contract=delivery-run, ` +
    `schema_version=1, profile=software, risk_tier=${receiptRisk}, status=executing, approved intent/design/authority evidence. ` +
    `Fill every risk_assessment factor from config/risk-policy.json conservatively; never lower the supplied tier. ` +
    `Fill authority from this human-requested task only: bounded source/artifact paths (when the run dir is inside repoRoot, ` +
    `artifact_write_paths must include that exact repo-relative run-dir subtree), expiry, prohibited paths/actions, ` +
    `external_disclosure, secrets, deployment=false, irreversible_actions=false and explicit ignored_path_exemptions. Do not invent broader authority. ` +
    `Capture git HEAD before mutation as implementation.base_revision and return it as baseRevision; fill implementation.repo_root. ` +
    `Require a clean source baseline: if tracked or untracked source changes already exist, fail preflight instead of hiding them in preexisting_paths. ` +
    `Bind the named human approval to matching human evidence and the canonical spec digest. ` +
    `Run deliver/scripts/validate_delivery.py <RUN.json> --workspace-root <repo-root>; return riskPreflightPassed=true only on exit 0, and return the receipt risk as effectiveRisk. ` +
    `Set assurance evaluation_required when behaviour is AI/stochastic/judgement-bearing; otherwise explain not-required. ` +
    `Set pair.mode=solo for this single-lead workflow. ` +
    `design status=${designStatus}, updated_at, and checkpoint generation=0/current_slice=bootstrap/` +
    `next_action=understand/in_flight=[]/artifact_paths=[RUN.json]. Leave implementation, verification and ` +
    `context_hygiene pending.\n` +
    `2. Identify the project root that the task targets and report it as repoRoot. Task: """${task}"""\n` +
    '3. Find a git repo dir usable as cwd for codex cross-family dispatch (report gitCwd; "" if none).\n' +
    '4. Discover conventions WITHOUT assuming a stack: read Makefile, package.json, pyproject, AGENTS.md, ' +
    'CLAUDE.md, README. Report the NARROWEST meaningful test lane, lint/type cmd, formatter, any design ' +
    'system entry point, and any rules that forbid auto-editing specific files.\n' +
    '5. Resolve concrete Claude models by running the global model router four times:\n' +
    '   provenant route resolve --adapter claude --alias flagship --role lead\n' +
    '   provenant route resolve --adapter claude --alias flagship --role critical-review\n' +
    '   provenant route resolve --adapter claude --alias workhorse --role worker\n' +
    '   provenant route resolve --adapter claude --alias scout --role scout\n' +
    '   Return each resolved_model in modelRoutes.\n' +
    'Return the structured result only.',
  { label: 'bootstrap', phase: 'Bootstrap', schema: BOOTSTRAP_SCHEMA },
)

if (!boot || !boot.runDir) {
  log('Bootstrap failed — cannot scaffold a run dir. Stopping; no edits made.')
  return
}
const runDir = boot.runDir
const gitCwd = boot.gitCwd || ''
const conv = boot.conventions || {}
const models = boot.modelRoutes
if (!boot.riskPreflightPassed || !['substantial', 'crucial', 'terminal'].includes(boot.effectiveRisk)) {
  log('Risk/authority preflight failed. No source mutation is authorised.')
  return
}
const effectiveRisk = boot.effectiveRisk
const maxRepairCycles = REPAIR_BUDGETS[effectiveRisk]
log(`Run dir: ${runDir} | repo: ${boot.repoRoot} | test lane: ${conv.testLane || '(none found)'}`)

const CHECKPOINT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['path', 'generation', 'verified'],
  properties: {
    path: { type: 'string' },
    generation: { type: 'integer' },
    verified: { type: 'boolean' },
  },
}
function shellQuote(value) {
  return "'" + String(value).replaceAll("'", "'\\''") + "'"
}
async function checkpoint(currentSlice, nextAction, inFlight, artifactPaths) {
  const result = await agent(
    `Run the deterministic checkpoint updater; do not edit JSON yourself or touch source:\n` +
      `python3 "$(provenant root)/skills/implement/scripts/checkpoint_run.py" "${runDir}/RUN.json" ` +
      `--current-slice ${JSON.stringify(currentSlice)} --next-action ${JSON.stringify(nextAction)} ` +
      `--in-flight-json ${shellQuote(JSON.stringify(inFlight))} --artifact-paths-json ${shellQuote(JSON.stringify(artifactPaths))}\n` +
      'Return its JSON stdout exactly as the structured result. A non-zero exit is failure.',
    { label: `checkpoint:${currentSlice}`, phase: 'Bootstrap', schema: CHECKPOINT_SCHEMA, model: models.scout },
  )
  if (!result || !result.verified) {
    log(`Checkpoint failed at ${currentSlice}; refusing the next dispatch.`)
    throw new Error(`recovery checkpoint failed at ${currentSlice}`)
  }
  return result
}
async function failRun(reason) {
  const result = await agent(
    `Close the failed workflow run at ${runDir} without touching source. Update RUN.json checkpoint to ` +
      `current_slice=failed, next_action=human inspect failure receipt, in_flight=[], and add the reason to ` +
      `unresolved_blockers. Classify every run artifact in MANIFEST.md with stable IDs/status/retention. Update ` +
      `RUN_RECEIPT.json task/owner and clear or hand off owned panes, then run run_dir_finalize.py --status failed ` +
      `--reason ${JSON.stringify(reason)}. Re-open both receipts and return path, generation and verified=true only ` +
      'when the run is terminal failed with empty in_flight.',
    { label: 'run:fail', phase: 'Human gate', schema: CHECKPOINT_SCHEMA, model: models.scout },
  )
  if (!result || !result.verified) {
    log('Failure terminalisation could not be verified; preserve the active run for manual recovery.')
    throw new Error('failed run terminalisation was not verified')
  }
  return result
}

// --- Phase 1: understand. Parallel Claude readers at distinct lenses + a cross-family explorer. ---
// parallel() here because phase 2 planning needs ALL reader findings together to frame the work.
phase('Understand')
// model omitted on every angle: inherit the session model (no dated literals, §3/§4). `tier` is intent only.
const READER_ANGLES = [
  { angle: 'data-flow', model: models.workhorse, cf: false, lens: 'trace the data/control flow the task touches end to end' },
  { angle: 'callers-and-contracts', model: models.workhorse, cf: false, lens: 'map callers, public surfaces, and any contracts/routes/schemas at risk' },
  { angle: 'tests-and-conventions', model: models.scout, cf: false, lens: 'find existing tests and the conventions a change must honour' },
  { angle: 'ui-and-design', model: models.workhorse, cf: false, lens: 'assess UI/design-system impact (skip if non-UI) honouring ' + (conv.designSystem || 'the project design system') },
  { angle: 'other-primary-explore', model: models.scout, cf: true, lens: 'drive an independent OpenAI-primary read through the dispatcher' },
]
await checkpoint('understand-dispatch', 'reconcile understanding workers', READER_ANGLES.map((r) => `understand:${r.angle}`), ['RUN.json'])

const understanding = (
  await parallel(
    READER_ANGLES.map((r) => () =>
      agent(
        `Understand this task for an upcoming change, from the "${r.angle}" angle: ${r.lens}.\n` +
          `Task: """${task}"""\nRepo root: ${boot.repoRoot}\nConventions: ${JSON.stringify(conv)}\n` +
          WORKER_CONTRACT +
          `\nWrite full notes to ${runDir}/findings/understand-${r.angle}.md.` +
          (r.cf ? '\n' + crossFamilyDispatchHint(runDir, gitCwd, 'primary') + ' Summarise the other-primary read in your findings.' : ''),
        { label: `understand:${r.angle}`, phase: 'Understand', schema: UNDERSTAND_SCHEMA, model: r.model },
      ),
    ),
  )
).filter(Boolean)
log(`Understand: ${understanding.length}/${READER_ANGLES.length} reader angles returned.`)
await checkpoint('understand-complete', 'plan against the approved contract', [], understanding.map((r) => r.path).filter(Boolean))

// --- Phase 2: multi-angle plan. Independent framings in parallel, then flagship adjudicates. ---
// parallel() because adjudication weighs ALL framings against each other (cross-item comparison).
phase('Plan')
const PLAN_FRAMINGS = [
  { framing: 'minimal-diff', lens: 'the smallest correct change; least blast radius' },
  { framing: 'robust-refactor', lens: 'fix the root cause cleanly even if the diff is larger' },
  { framing: 'ux-first', lens: 'optimise the user-facing/behavioural outcome (use only if the task has a UX surface; else say so)' },
]
const understandingDigest = JSON.stringify(
  understanding.map((u) => ({ angle: u.angle, findings: u.findings, touchedFiles: u.touchedFiles, risks: u.risks })),
)
await checkpoint('plan-dispatch', 'reconcile independent plan framings', PLAN_FRAMINGS.map((p) => `plan:${p.framing}`), [])
const plans = (
  await parallel(
    PLAN_FRAMINGS.map((p) => () =>
      agent(
        `Produce an INDEPENDENT plan for this task using the "${p.framing}" framing: ${p.lens}.\n` +
          `Task: """${task}"""\nUser risk hint: ${riskHint}\nUnderstanding: ${understandingDigest}\n` +
          ESCALATION_RULES +
          '\nDo not read the other framings; argue your own. Predict the riskTier of the resulting edits. ' +
          'Return the structured plan only.',
        { label: `plan:${p.framing}`, phase: 'Plan', schema: PLAN_SCHEMA, model: models.workhorse },
      ),
    ),
  )
).filter(Boolean)

// Flagship orchestrator weighs the framings — does NOT majority-vote. Inherits session (opus) model.
await checkpoint('plan-adjudication-dispatch', 'reconcile the selected plan', ['plan:adjudicate'], [])
const chosen = await agent(
  'Adjudicate competing plan framings for this task. Weigh them on correctness, blast radius, and fit ' +
    'to the discovered conventions — do NOT majority-vote. Pick one (or a justified synthesis), and ' +
    'restate its steps + files + riskTier.\n' +
    `Task: """${task}"""\nFramings: ${JSON.stringify(plans)}\n` +
    ESCALATION_RULES +
    `\nWrite the decision to ${runDir}/findings/plan-decision.md and return the structured choice.`,
  { label: 'plan:adjudicate', phase: 'Plan', schema: CHOSEN_PLAN_SCHEMA, model: models.flagship },
)
if (!chosen) {
  log('Planning produced no viable framing — stopping before implement. No edits made.')
  await failRun('planning produced no viable framing')
  return
}
log(`Chosen framing: ${chosen.chosenFraming} (predicted ${chosen.riskTier}-risk).`)
await checkpoint('plan-complete', 'implement partitioned patches', [], [chosen.path].filter(Boolean))

// --- Phase 3: implement. One builder emits PATCHES (no direct source edits — non-git-safe). mid tier. ---
phase('Implement')
await checkpoint('implement-dispatch', 'reconcile emitted patches', ['implement'], [])
let built = await agent(
  'Implement the chosen plan, but DO NOT edit source files. Instead emit each change as a unified-diff ' +
    `patch file under ${runDir}/patches/, self-tagging its risk tier per the rules below.\n` +
    `Task: """${task}"""\nChosen plan: ${JSON.stringify(chosen)}\nConventions: ${JSON.stringify(conv)}\n` +
    `Repo root: ${boot.repoRoot}\n` +
    'Before emitting a patch, name the governing skill for this change in the implementation narrative; ' +
    'for every new or changed observable behaviour that skill must include `tdd` and a right-reason red. ' +
    ESCALATION_RULES +
    '\nKeep patches minimal and self-contained; one logical change per patch so the applier can land ' +
    'low-risk ones independently. Add a brief code comment for any unobvious decision.\n' +
    'CRITICAL patch-only contract: you have file-edit tools but you MUST NOT mutate any source file. ' +
    'Write ONLY to the run dir (patches/ + findings/). Before you finish, if the repo root is a git ' +
    'tree, run `git -C <repo-root> status --porcelain` and confirm it reports NO source changes (only ' +
    'run-dir files, which sit outside the repo, should differ). If you accidentally touched a source ' +
    'file, preserve the unexpected diff, stop, and report the exact paths for human recovery. Never ' +
    'checkout, restore, reset, or overwrite a dirty file. ' +
    `Write the build narrative to ${runDir}/findings/implement.md and return the structured result.`,
  { label: 'implement', phase: 'Implement', schema: IMPLEMENT_SCHEMA, model: models.workhorse },
)
if (!built || !built.patches || built.patches.length === 0) {
  log('Implement produced no patches — nothing to review/apply. Stopping.')
  await failRun('implementation produced no patches')
  return
}
log(`Implement: ${built.patches.length} patch(es) emitted.`)
await checkpoint('implement-complete', 'run independent review and verification', [], [built.path, ...built.patches.map((p) => p.patchPath)].filter(Boolean))

// --- Phases 4-5: review + verify under the risk-tier repair budget. ---
const REVIEW_ANGLES = [
  { angle: 'correctness', model: models.criticalReviewer, cf: '', required: true, lens: 'correctness, invariants and failure paths' },
  { angle: 'regression-and-structure', model: models.criticalReviewer, cf: '', required: true, lens: 'dependency-cone regressions, ownership, state/types, atomicity and simplification' },
  { angle: 'scope-and-risk', model: models.criticalReviewer, cf: '', required: true, lens: 'scope drift, risk tier and authority boundaries' },
  { angle: 'other-primary', model: models.scout, cf: 'primary', required: true, lens: 'drive an independent OpenAI-primary adversarial review' },
  { angle: 'distinct-family', model: models.scout, cf: 'distinct-family', required: false, lens: 'distinct-family blind-spot pressure when warranted' },
]
let reviews = []
let verify = null
let blocking = 0
let otherPrimaryRan = false
let distinctFamilyRan = false
let checksPass = false
let repairCycles = 0
let patchDigest = ''

for (let cycle = 0; cycle <= maxRepairCycles; cycle += 1) {
  patchDigest = JSON.stringify(built.patches)
  phase('Review')
  await checkpoint(`review-${cycle}-dispatch`, 'reconcile reviewer results', REVIEW_ANGLES.map((r) => `review:${cycle}:${r.angle}`), [])
  const rawReviews = await parallel(
    REVIEW_ANGLES.map((rv) => () =>
      agent(
        'Load the installed code-review skill and its required references. ' +
          `Review cycle ${cycle}, angle "${rv.angle}": ${rv.lens}. Read-only; the diff is entrypoint, not boundary.\n` +
          `Task: """${task}"""\nAcceptance criteria: ${JSON.stringify(acceptanceCriteria)}\n` +
          `Patches: ${patchDigest}\nConventions: ${JSON.stringify(conv)}\n` +
          ESCALATION_RULES +
          '\nFor a native Claude review, set crossFamily to ran=false, tool="", status="not-applicable", ' +
          'modelFamily="anthropic", endpointProvider="anthropic", crossFamily=false, certificationEligible=false, readOnlyGuarantee="none", ' +
          'outputPath="", routeReceipt="", notRunReason="targeted-review". For a dispatched review, copy every normalised field ' +
          'from the dispatcher record; do not infer or relabel lineage.\n' +
          `\nWrite full review to ${runDir}/findings/review-${cycle}-${rv.angle}.md and return the structured verdict.` +
          (rv.cf ? '\n' + crossFamilyDispatchHint(runDir, gitCwd, rv.cf === 'primary' ? 'primary-review' : rv.cf) : ''),
        { label: `review:${cycle}:${rv.angle}`, phase: 'Review', schema: REVIEW_SCHEMA, model: rv.model },
      ),
    ),
  )
  reviews = rawReviews.map((result, index) =>
    result || {
      angle: REVIEW_ANGLES[index].angle,
      verdict: REVIEW_ANGLES[index].required ? 'block' : 'approve-with-nits',
      issues: REVIEW_ANGLES[index].required
        ? [{ severity: 'P1', patchPath: '', detail: 'required review lane failed or returned no result' }]
        : [],
      crossFamily: { ran: false, tool: '', status: 'unavailable', modelFamily: '', endpointProvider: '', crossFamily: false, certificationEligible: false, readOnlyGuarantee: 'none', outputPath: '', routeReceipt: '', notRunReason: REVIEW_ANGLES[index].required ? 'review-lane-failed' : 'distinct-family-review-unavailable' },
      path: '',
    },
  )
  const distinctFamilyReview = reviews.find((r) => r.angle === 'distinct-family')
  if (distinctFamilyReview && distinctFamilyReview.crossFamily && distinctFamilyReview.crossFamily.ran && distinctFamilyReview.issues.length > 0) {
    const corroborated = await agent(
      'Corroborate the distinct-family review against the actual patches, repository context and acceptance criteria. ' +
      'Treat it as a lead, not authority. Return block only for a reproducible, task-relevant defect supported by ' +
        'primary evidence; otherwise approve or approve-with-nits. This is a native Claude corroboration: set ' +
        'crossFamily to ran=false, tool="", status="not-applicable", modelFamily="anthropic", ' +
        'endpointProvider="anthropic", crossFamily=false, certificationEligible=false, readOnlyGuarantee="none", outputPath="", routeReceipt="", ' +
        'notRunReason="native-corroboration".\n' +
        `Distinct-family review: ${JSON.stringify(distinctFamilyReview)}\nPatches: ${patchDigest}\nTask: ${task}`,
      { label: `review:${cycle}:distinct-family-corroboration`, phase: 'Review', schema: REVIEW_SCHEMA, model: models.criticalReviewer },
    )
    if (corroborated) reviews.push(corroborated)
  }
  blocking = reviews.filter((r, index) =>
    (index >= REVIEW_ANGLES.length || REVIEW_ANGLES[index].required) && r.verdict === 'block'
  ).length
  otherPrimaryRan = reviews.some((r) => r.angle === 'other-primary' && r.crossFamily && r.crossFamily.ran && r.crossFamily.status === 'ok' && r.crossFamily.crossFamily && r.crossFamily.certificationEligible && ['enforced', 'oauth_safe_mode'].includes(r.crossFamily.readOnlyGuarantee))
  distinctFamilyRan = reviews.some((r) => r.angle === 'distinct-family' && r.crossFamily && r.crossFamily.ran && r.crossFamily.status === 'ok')
  log(`Review cycle ${cycle}: ${reviews.length} verdicts, ${blocking} blocking; other-primary ${otherPrimaryRan ? 'ran' : 'NOT run'}; distinct-family ${distinctFamilyRan ? 'ran' : 'not available'}.`)
  await checkpoint(`review-${cycle}-complete`, 'run objective verification', [], reviews.map((r) => r.path).filter(Boolean))

  phase('Verify')
  await checkpoint(`verify-${cycle}-dispatch`, 'reconcile objective verification', [`verify:${cycle}`], [])
  verify = await agent(
    'Run objective verification for the proposed patches on the narrowest meaningful lane. Apply patches ' +
      'only to a scratch copy made without the source .git metadata; initialise fresh Git metadata if needed. ' +
      'Never land them in the real tree. Map every acceptance criterion ' +
      'to evidence and report skipped/unavailable checks as failures.\n' +
      `Acceptance criteria: ${JSON.stringify(acceptanceCriteria)}\n` +
      `Discovered test lane: ${conv.testLane || '(none)'}\nLint/format: ${conv.lintCmd || '(none)'} / ${conv.formatCmd || '(none)'}\n` +
      `Patches: ${patchDigest}\nRepo root: ${boot.repoRoot}\n` +
      `Write evidence to ${runDir}/traces/verify-${cycle}.md and return the structured outcome.`,
    { label: `verify:${cycle}`, phase: 'Verify', schema: VERIFY_SCHEMA, model: models.workhorse },
  )
  checksPass = !!(verify && verify.passed)
  await checkpoint(`verify-${cycle}-complete`, checksPass && blocking === 0 ? 'prepare serial apply' : 'repair blocking findings', [], [verify && verify.path].filter(Boolean))
  if (checksPass && blocking === 0) break
  if (cycle === maxRepairCycles) break

  phase('Repair')
  await checkpoint(`repair-${cycle + 1}-dispatch`, 'reconcile replacement patches', [`repair:${cycle + 1}`], [])
  const repaired = await agent(
    'Repair the proposed patch set from the supported review findings and failed verification. Do not edit ' +
      'the real source tree. Emit a complete replacement patch set under the run directory, preserving ' +
      'approved behaviour and scope. Do not guess around a spec/design defect; report it instead.\n' +
      `Task: ${task}\nPatches: ${patchDigest}\nReviews: ${JSON.stringify(reviews)}\nVerify: ${JSON.stringify(verify)}\n` +
      `Write the repair narrative to ${runDir}/findings/repair-${cycle + 1}.md.`,
    { label: `repair:${cycle + 1}`, phase: 'Repair', schema: IMPLEMENT_SCHEMA, model: models.workhorse },
  )
  if (!repaired || !repaired.patches || repaired.patches.length === 0) {
    log('Repair produced no replacement patches; stopping at the human gate.')
    break
  }
  built = repaired
  await checkpoint(`repair-${cycle + 1}-complete`, 'repeat independent review and verification', [], [repaired.path, ...repaired.patches.map((p) => p.patchPath)].filter(Boolean))
  repairCycles = cycle + 1
}

// Evidence council: first opinions above were blind/parallel. Strip author/provider identity,
// challenge claims in a separate context, then let a fresh reducer adjudicate from evidence.
const anonymousClaims = reviews.map((review, index) => ({
  claimId: `C${index + 1}`,
  lens: review.angle,
  verdict: review.verdict,
  issues: review.issues,
  artifact: review.path || (review.crossFamily && review.crossFamily.outputPath) || '',
})).reverse()
const councilChallenge = await agent(
  'Challenge these anonymised, order-scrambled review claims against the patches, approved criteria and repository evidence. ' +
    'Do not infer authors or vote. Identify unsupported, duplicate, missed and contradictory claims. ' +
    `Claims: ${JSON.stringify(anonymousClaims)}\nPatches: ${patchDigest}\nTask: ${task}\n` +
    `Write the challenge to ${runDir}/findings/council-challenge.md and return anonymized=true, randomized=true and its path.`,
  { label: 'review:council-challenge', phase: 'Review', schema: COUNCIL_CHALLENGE_SCHEMA, model: models.criticalReviewer },
)
const councilReduction = await agent(
  'Act as a fresh-context evidence reducer. Read the blind review artifacts and the anonymised challenge. ' +
    'Adjudicate each claim using source/spec/test evidence, never majority vote, and preserve unresolved dissent. ' +
    `Reviews: ${JSON.stringify(anonymousClaims)}\nChallenge: ${JSON.stringify(councilChallenge || {})}\n` +
    `Write the reduction to ${runDir}/findings/council-reduction.md.`,
  { label: 'review:council-reduction', phase: 'Review', schema: COUNCIL_REDUCTION_SCHEMA, model: models.flagship },
)
if (!councilChallenge || !councilChallenge.anonymized || !councilChallenge.randomized ||
    !councilReduction || !councilReduction.freshContext || councilReduction.unresolvedDissent.length > 0) {
  blocking += 1
}

log(`Final machine checks: ${checksPass ? 'PASS' : 'FAIL/UNKNOWN'}; blocking reviews: ${blocking}; repair cycles: ${repairCycles}.`)

// --- Phase 6: apply gate. SINGLE SERIAL applier. Auto-apply low-risk iff checks pass + no block; ---
// --- escalate everything else as a validated recommendation. NEVER blocks for approval. ---
phase('Apply')
// Auto-apply policy (explicit): gate low-risk auto-apply on objective checks passing AND no reviewer
// block. Cross-family NOT running is a recorded outcome (CROSS-FAMILY-NOT-RUN), NOT a hard block on
// low-risk auto-apply — but we surface it so the skipped decorrelated review is a conscious record,
// per the orchestrate skill's recovery doctrine ("never silently skip a verification step").
const autoApplyAllowed = checksPass && blocking === 0 && otherPrimaryRan && ['substantial', 'crucial'].includes(effectiveRisk)
if (autoApplyAllowed && !otherPrimaryRan) {
  log('Auto-apply proceeding only for routine LOW-risk patches without an other-primary review; native reviews passed.')
}
await checkpoint('apply-dispatch', 'reconcile serial apply and final machine gate', ['apply:serial'], [])
const apply = await agent(
  'You are the SINGLE SERIAL applier — the only agent allowed to mutate the real tree, applied one ' +
    'patch at a time (avoids concurrent-write corruption on non-git trees).\n' +
    `Auto-apply permitted this run: ${autoApplyAllowed} (objective checks passed, no reviewer blocked, ` +
    `and required other-primary coverage ran for this risk tier). Other-primary ran: ${otherPrimaryRan}; ` +
    `distinct-family availability never replaces other-primary coverage; ran: ${distinctFamilyRan}.\n` +
    'Pre-apply freshness guard (do this BEFORE applying anything):\n' +
    '- If the repo root is a git tree, run `git -C <repo-root> status --porcelain`. Each patch targets ' +
    'specific files; for every target file that already shows uncommitted local changes, DO NOT apply ' +
    'that patch — escalate it as "target drifted (dirty worktree); needs manual rebase". A clean target ' +
    'is required because the patches were built against the worktree at implement time.\n' +
    '- For each candidate patch, run `git -C <repo-root> apply --check <patch>` first. If it does not ' +
    'apply cleanly, DO NOT force it — escalate it with the --check error as the reason.\n' +
    'Rules:\n' +
    '- Apply LOW-risk patches, plus HIGH-risk patches only when effectiveRisk=crucial and the preflight authority expressly covers every path/action. ' +
    'Terminal, destructive, irreversible, deployment or externally communicating patches always escalate. In every case auto-apply must be permitted, the target clean, ' +
    'and `git apply --check` passed. After each apply, re-run the relevant narrow check; if it fails, ' +
    'reverse only that just-applied patch with `git apply -R`; never checkout, restore, reset or overwrite ' +
    'a pre-existing dirty file. Then escalate it.\n' +
    '- Leave terminal/destructive/irreversible high-risk patches in patches/ unapplied; crucial high-risk patches follow the scoped rule above.\n' +
    ESCALATION_RULES +
    `\nPatches: ${patchDigest}\nReview verdicts and dispatcher lineage: ${JSON.stringify(reviews)}\n` +
    `Verify outcome: ${JSON.stringify(verify || {})}\nRepo root: ${boot.repoRoot}\n` +
    `Before mutating, read the existing live ${runDir}/RUN.json receipt created at bootstrap; do not recreate or replace it. ` +
    `For effectiveRisk=terminal, apply nothing: preserve recommendation evidence, leave status=executing/current_slice=awaiting-apply-approval, ` +
    `return machineGatePassed=false, and do not pretend the final implementation gate ran. ` +
    `For non-terminal runs only, after all patch apply/escalate decisions and narrow re-checks, update status=awaiting_acceptance, the human-approved ` +
    `spec/design status, risk/authority profile, assurance receipt/status, acceptance criteria with evidence, ` +
    `implementation outcome with repo_root=${boot.repoRoot}, base_revision=${boot.baseRevision}, preexisting_paths=[], ` +
    `every applied path's add|modify|delete operation and SHA-256, and the validator's canonical result_revision; exact verification results, ` +
    `repair_cycles=${repairCycles}, current_slice, next_action, empty in_flight IDs and artifact_paths. ` +
    `Then run the global session context_audit.py read-only and record context_hygiene status, audit command + exit code, ` +
    `graduation/archive/cleanup actions and retained recovery artifacts. Never remove unknown or pre-existing files. ` +
    `Update ${runDir}/RUN_RECEIPT.json task/owner, artifact retention and owned/handed-off pane fields; leave its ` +
    `status=active while this change awaits human acceptance. Record unresolved blockers and every reviewer lane ` +
    `including failures. Every complete review_plan row must preserve its explicit wrapper verdict and the ` +
    `exact worker/provider REVIEW_SCHEMA object at the existing dispatcher output_path, retaining its ` +
    `run-relative path and sha256 digest as terminal_result alongside the dispatcher route receipt's observed exit and ` +
    `output_path and dispatch-time output_digest. Never inline, synthesise or derive terminal_result or its verdict from the wrapper row, ` +
    `transcript, findings, or applier analysis. Missing or unparseable worker verdict, transcript, provider availability or ` +
    `nonzero exit is an explicit non-certifying leg. The preserved cross-family dispatch record supplies ` +
    `adapter, endpoint_provider, provider_family, model_family, orchestrator_family, output_path, dispatch status ` +
    `and read_only_guarantee; derive cross-family eligibility from that lineage and the chair family, not from ` +
    `cross_family or certification_eligible booleans; ` +
    `role=targeted for fresh targeted lenses, role=other-primary only for a certified ` +
    `OpenAI-family reviewer with its exact route_receipt path, output sha256 and reviewed_revision, and role=distinct-family for advisory distinct-family attempts including failed/unavailable ` +
    `status plus reason. For terminal work, apply stronger targeted and adversarial pressure; if a distinct family is skipped, record distinct_family_coverage_reason. ` +
    `Distinct-family failure never replaces other-primary coverage. Populate review_council from at least two distinct blind ` +
    `targeted/other-primary review artifacts, ${JSON.stringify(councilChallenge || {})}, and ${JSON.stringify(councilReduction || {})}; ` +
    `record distinct paths, output SHA-256 values, actor family/adapter/review role, final reviewed_revision and post_repair_review; ` +
    `name the correctness lens exactly correctness-spec. Run ` +
    '"$(provenant root)/skills/deliver/scripts/validate_delivery.py" ' + `"${runDir}/RUN.json" --workspace-root "${gitCwd}" --verify-hashes. ` +
    `If validation fails, machineGatePassed=false, stop further mutation and escalate with the validator output; ` +
    `preserve any already-applied, independently checked low-risk patch honestly.\n` +
    `Update ${runDir}/MANIFEST.md with every run artifact after serial work. Use stable artifact IDs, ` +
    `status=draft|verified|superseded|retired, retention=capsule|evidence|ephemeral, and supersedes=<artifact ID|->. ` +
    `Record applied/escalated as topic/outcome, not as manifest status. For escalated patches, write a ` +
    `fully-reasoned approve-then-apply recommendation to ${runDir}/RECOMMENDATION.md (what, why high-risk, ` +
    'validation evidence, the exact approval gate it needs). Return the structured result.',
  { label: 'apply:serial', phase: 'Apply', schema: APPLY_SCHEMA, model: models.workhorse },
)

const appliedN = apply && apply.applied ? apply.applied.length : 0
const escalatedN = apply && apply.escalated ? apply.escalated.length : 0
log(`Apply: ${appliedN} low-risk patch(es) landed, ${escalatedN} escalated for approval.`)
if (escalatedN > 0 && apply) {
  log(`Escalation recommendation: ${apply.recommendationPath}. Run a separate approve-then-apply step.`)
}
if (effectiveRisk === 'terminal' && appliedN === 0 && escalatedN > 0) {
  phase('Human gate')
  log('Terminal-risk change is awaiting explicit apply authority; this is not a failed implementation or final acceptance gate.')
  return {
    task, runId, runDir, chosenFraming: chosen.chosenFraming, patchesEmitted: built.patches.length,
    checksPass, blockingReviews: blocking, otherPrimaryReviewRan: otherPrimaryRan,
    distinctFamilyReviewRan: distinctFamilyRan, repairCycles, state: 'awaiting-apply-approval',
    applied: appliedN, escalated: escalatedN, manifest: apply.manifestPath,
    recommendation: apply.recommendationPath, runReceipt: apply.runPath, machineGatePassed: false,
  }
}
if (!apply || !apply.machineGatePassed) {
  await failRun(apply ? 'machine lifecycle gate failed' : 'serial apply returned no result')
  phase('Human gate')
  log('Machine gate FAILED. The run is terminal failed with recovery evidence; do not present it for acceptance.')
  return {
    task,
    runId,
    runDir,
    chosenFraming: chosen.chosenFraming,
    patchesEmitted: built.patches.length,
    checksPass,
    blockingReviews: blocking,
    otherPrimaryReviewRan: otherPrimaryRan,
    distinctFamilyReviewRan: distinctFamilyRan,
    repairCycles,
    state: 'failed',
    applied: appliedN,
    escalated: escalatedN,
    manifest: apply ? apply.manifestPath : `${runDir}/MANIFEST.md`,
    recommendation: apply ? apply.recommendationPath : null,
    runReceipt: apply ? apply.runPath : `${runDir}/RUN.json`,
    machineGatePassed: false,
  }
}

phase('Human gate')
log('Machine work is awaiting human acceptance. Do not mark the change complete until the human approves it.')

return {
  task,
  runId,
  runDir,
  chosenFraming: chosen.chosenFraming,
  patchesEmitted: built.patches.length,
  checksPass,
  blockingReviews: blocking,
  otherPrimaryReviewRan: otherPrimaryRan,
  distinctFamilyReviewRan: distinctFamilyRan,
  repairCycles,
  state: 'awaiting-human',
  applied: appliedN,
  escalated: escalatedN,
  manifest: apply ? apply.manifestPath : `${runDir}/MANIFEST.md`,
  recommendation: apply ? apply.recommendationPath : null,
  runReceipt: apply ? apply.runPath : `${runDir}/RUN.json`,
  machineGatePassed: !!(apply && apply.machineGatePassed),
}
