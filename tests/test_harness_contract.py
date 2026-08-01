from pathlib import Path
import re
import shutil
import subprocess
import sys

import pytest
import yaml

from scripts.count_skill_words import count_skill_words


ROOT = Path(__file__).resolve().parents[1]
SKILL_COUNT = len(list((ROOT / "skills").glob("*/SKILL.md")))
WORKFLOWS = ROOT / "workflows"


def frontmatter_name(path: Path) -> str:
    text = path.read_text()
    match = re.search(r"^name:\s*([^\n]+)$", text, re.MULTILINE)
    assert match, f"missing name in {path}"
    return match.group(1).strip()


def test_lifecycle_skills_are_portable_and_named_for_their_directory():
    for name in ("implement", "code-review"):
        skill = ROOT / "skills" / name / "SKILL.md"
        assert skill.is_file(), f"missing portable {name} skill"
        assert frontmatter_name(skill) == name


def test_claude_workflows_do_not_reference_retired_orchestration_skill():
    workflows = list(WORKFLOWS.glob("*.js"))
    assert workflows, "no in-repo workflows found; the guard must not pass vacuously"
    offenders = [
        path.name
        for path in workflows
        if "multi-agent-orchestration" in path.read_text()
    ]
    assert offenders == []


def test_constitution_names_equal_primaries_and_router_has_current_codex_family():
    text = (ROOT / "HARNESS.md").read_text()
    assert "fallback orchestrator" not in text.lower()
    assert all(alias in text for alias in ("flagship", "workhorse", "scout"))
    assert "equal primary orchestrators" in text
    assert "other-primary leg remains required" in text
    assert "multiple targeted lenses plus strong other-primary" in text
    assert "stronger targeted and adversarial pressure" in text
    routing = (ROOT / "skills" / "orchestrate" / "references" / "routing-and-tiers.md").read_text()
    assert "GPT-5.6" in routing


def test_first_use_fabric_trust_is_exact_and_does_not_provision():
    agents = " ".join((ROOT / "AGENTS.md").read_text().split())
    runbook = " ".join(
        (ROOT / "docs" / "runbooks" / "agent-fabric-operations.md").read_text().split()
    )
    assert "exact canonical repository root" in agents
    assert "never trust a parent, wildcard, home or sibling collection" in agents
    assert "Standing global harness authority covers only automatic trust registration" in runbook
    assert "Seat provisioning remains separately gated" in runbook
    assert "trust a project root or provision" not in runbook


def test_first_use_fabric_trust_uses_a_path_resolved_invocation():
    # D4: ambient files carry no location-bearing launcher path; the Fabric-trust
    # bullet uses the PATH-resolved `provenant fabric workspace trust`. The
    # repo-scoped runbook may still spell the explicit home launcher.
    agents = " ".join((ROOT / "AGENTS.md").read_text().split())
    runbook = " ".join(
        (ROOT / "docs" / "runbooks" / "agent-fabric-operations.md").read_text().split()
    )
    assert "provenant fabric workspace trust" in agents
    assert "$HOME/.agents/scripts/agent-fabric" not in agents
    assert '$HOME/.agents/scripts/agent-fabric workspace trust "$project_root"' in runbook
    assert '`scripts/agent-fabric workspace trust "$project_root"`' not in runbook


def test_first_use_fabric_docs_explain_bootstrap_trust_recovery():
    agents = " ".join((ROOT / "AGENTS.md").read_text().split())
    readme = " ".join((ROOT / "README.md").read_text().split())
    assert "WORKSPACE_NOT_TRUSTED" in agents
    assert "retry `fabric_bootstrap`" in agents
    assert "WORKSPACE_NOT_TRUSTED" in readme
    assert "provenant fabric workspace trust" in readme


def test_subagent_dispatch_contract_requires_task_class_bound_route_and_receipt():
    harness = (ROOT / "HARNESS.md").read_text()
    skill = (ROOT / "skills" / "orchestrate" / "SKILL.md").read_text()
    contract = (
        ROOT / "skills" / "orchestrate" / "references" / "orchestration-contract.md"
    ).read_text()
    codex = (
        ROOT / "skills" / "orchestrate" / "references" / "codex-subagents.md"
    ).read_text()
    for field in ("task class", "tier", "model", "effort", "route receipt", "governing skill"):
        assert field in contract.lower()
    assert "task class" in skill.lower()
    assert "governing skill" in contract.lower()
    # HARNESS keeps the full routing invariant (route by task class, bind identity +
    # effort + receipt, runtime governs); the Codex subscription-native
    # effort-binding detail lives in the orchestrate reference below (D2 routing
    # depth -> orchestrate). Assert the invariant as contiguous phrases so trimming
    # a bound field (e.g. "binding identity") cannot pass.
    assert "Route every dispatch by task class" in harness
    assert "binding identity, effort and receipt" in harness
    assert "runtime governs" in harness
    assert (
        "For subscription-native Codex workers, omit the literal transport `model` "
        "and bind the resolved `effort`."
    ) in codex
    assert "governing-skill" in codex


def test_ambient_files_meet_the_static_disclosure_gates():
    # AC-S1 hard gates (D1/D4/D10/D12): the whole constitution lives in the two
    # ambient files within fixed line caps, carries no date, no repo-relative
    # docs/config/scripts path and no skills/<x>/references path, and states its
    # skill-resolution root exactly once via the D12 resolver line. Nothing else
    # in the suite enforces the line caps, so this is the binding machine gate.
    import re

    agents = (ROOT / "AGENTS.md").read_text()
    harness = (ROOT / "HARNESS.md").read_text()
    # `wc -l` semantics: a trailing-newline file has exactly line-count newlines.
    assert agents.count("\n") <= 35, "AGENTS.md exceeds its 35-line hard cap (D1)"
    assert harness.count("\n") <= 60, "HARNESS.md exceeds its 60-line hard cap (D1)"

    forbidden_path = re.compile(r"(?:docs|config|scripts)/[A-Za-z]|skills/[a-z-]+/references")
    date = re.compile(r"20[0-9]{2}-[0-9]{2}-[0-9]{2}")
    for name, text in (("AGENTS.md", agents), ("HARNESS.md", harness)):
        assert not forbidden_path.search(text), f"{name} has a forbidden repo-relative/references path (D4/D3)"
        assert not date.search(text), f"{name} carries a date (D10)"
        # The D12 resolver names the skills root; it is the sole location-bearing line.
        assert "$HOME/.agents/skills/<name>/" in text
        assert "~/.codex/skills/" in text


def test_constitution_is_a_compact_core_with_progressive_disclosure():
    text = (ROOT / "HARNESS.md").read_text()
    assert len(text.split()) <= 700
    # Progressive disclosure is by skill NAME, never by a reference-file path (D3),
    # and the constitution carries no repo-relative config/ path (D4).
    assert "skills/orchestrate/references" not in text
    assert "paired-primary.md" not in text
    assert "config/risk-policy.json" not in text
    assert "Load depth only when triggered" in text
    for skill in ("`orchestrate`", "`implement`", "`deliver`", "`session`", "`release`", "`evaluate`"):
        assert skill in text


def test_release_and_evaluate_complete_the_delivery_spine():
    for name in ("release", "evaluate"):
        skill = ROOT / "skills" / name / "SKILL.md"
        assert skill.is_file()
        assert frontmatter_name(skill) == name


def test_autopilot_external_reviewers_are_fabric_first():
    skill = (ROOT / "skills" / "autopilot" / "SKILL.md").read_text()
    orchestration = (ROOT / "skills" / "orchestrate" / "SKILL.md").read_text()
    dispatcher = (ROOT / "skills" / "orchestrate" / "scripts" / "cf_dispatch.sh").read_text()
    assert not (ROOT / "skills" / "autopilot" / "scripts" / "cross-family.sh").exists()
    assert "Agent Fabric" in skill
    assert "Answer-bearing external work uses Fabric request/reply" in orchestration
    assert "codex exec -s read-only" in dispatcher
    assert "explicit degraded fallback" in dispatcher


def test_root_harness_checker_is_available():
    checker = ROOT / "scripts" / "check-harness"
    assert checker.is_file()
    assert checker.stat().st_mode & 0o111


def test_dispatchers_use_the_stable_product_command_and_local_skill_helpers():
    dispatcher = (ROOT / "skills" / "orchestrate" / "scripts" / "cf_dispatch.sh").read_text()
    assert 'resolve_routing' in dispatcher  # tries provenant, falls back to model_route.py
    assert '"$SCRIPT_DIR/codex_capabilities.py"' in dispatcher
    assert "AGENTS_ROOT" not in dispatcher
    assert "HARNESS_ROOT" not in dispatcher


@pytest.mark.skipif(
    not all((WORKFLOWS / name).is_file() for name in ("implement-run.js", "codebase-polish.js", "cross-verify.js")),
    reason="optional local Claude workflow installation is absent",
)
def test_claude_workflows_use_router_and_safe_implement_loop():
    for name in ("implement-run.js", "codebase-polish.js", "cross-verify.js"):
        text = (WORKFLOWS / name).read_text()
        assert "provenant route resolve" in text
        assert '${AGENTS_HOME:-$HOME/.agents}/scripts/model-route' not in text
        assert "claude-haiku-" not in text
    implementation = (WORKFLOWS / "implement-run.js").read_text()
    assert "cycle <= maxRepairCycles" in implementation
    assert "routine: 2" in implementation
    assert "substantial: 4" in implementation
    assert "crucial: 5" in implementation
    assert "terminal: 5" in implementation
    assert "state: 'awaiting-human'" in implementation
    assert "git checkout" not in implementation
    assert "git restore" not in implementation
    assert "git reset" not in implementation
    assert "required: false" in implementation
    assert "otherPrimaryRan" in implementation
    assert "distinct-family availability never replaces other-primary" in implementation
    assert "Copy the global deliver RUN.template.json" in implementation
    assert "refusing the next dispatch" in implementation
    assert "do not recreate or replace it" in implementation
    assert "Review verdicts and dispatcher lineage" in implementation
    assert "certification_eligible" in implementation
    assert "Machine gate FAILED" in implementation
    assert "state: 'failed'" in implementation


def test_implement_skill_uses_canonical_delivery_completion_states():
    text = (ROOT / "skills" / "implement" / "SKILL.md").read_text()
    assert "`awaiting_acceptance`" in text
    assert "`accepted`" in text
    assert "`complete` only after" not in text
    assert "`awaiting-human` is a successful machine-gate state" not in text
    assert not (ROOT / "skills" / "implement" / "scripts" / "validate_run.py").exists()
    assert not (ROOT / "skills" / "implement" / "templates" / "RUN.template.json").exists()


def test_read_only_review_allows_scoped_artifacts_but_forbids_unscoped_scratch():
    text = " ".join((ROOT / "skills" / "code-review" / "SKILL.md").read_text().split())
    implementation = " ".join((ROOT / "skills" / "implement" / "SKILL.md").read_text().split())
    verification = " ".join((ROOT / "skills" / "orchestrate" / "references" / "verification.md").read_text().split())
    assert "Artifact-only authority permits named outputs under the assigned run directory" in text
    assert "does not permit arbitrary repo-root scratch" in text
    assert "Reviewed source stays immutable" in implementation
    assert "namespaced review artifact and correlated Fabric" in verification


def test_code_review_uses_task_selected_multi_agent_lenses_without_voting():
    skill = (ROOT / "skills" / "code-review" / "SKILL.md").read_text()
    topology = (ROOT / "skills" / "code-review" / "references" / "multi-agent-review.md").read_text()
    assert "independent targeted agents" in skill
    assert "Correctness/spec alignment" in skill
    assert "anonymised claim challenge" in skill
    assert "Never rank prose or majority-vote" in skill
    assert "fresh-context reducer" in topology


def test_default_agent_run_directory_is_ignored_in_the_harness_repo():
    assert ".agent-run/" in (ROOT / ".gitignore").read_text().splitlines()


def test_private_receipts_project_durable_public_evidence_without_being_tracked():
    # The comprehensive-review programme restated this doctrine in its KICKOFF and
    # CHAIR-CHARTER. That programme is superseded and deleted (701d663); its durable
    # owner is the run contract, which is what this gate now binds.
    run_contract = (
        ROOT / "skills" / "implement" / "references" / "run-contract.md"
    ).read_text()
    maintaining = (ROOT / "MAINTAINING.md").read_text()

    for text in (run_contract,):
        assert "validator-readable" in text
        assert "force-track" in text
        assert "tested-tree facts" in text
    assert "scripts/public-release-check --publication-range" in maintaining
    assert "origin/main^{commit}" in maintaining
    assert "HEAD^{commit}" in maintaining


def test_context_hygiene_is_owned_by_session_and_delivery_checkpoints():
    harness = " ".join((ROOT / "HARNESS.md").read_text().split())
    implementation = (ROOT / "skills" / "implement" / "references" / "run-contract.md").read_text()
    assert "`session` owns context hygiene" in harness
    assert "RUN.json" in implementation
    assert "checkpoint" in (ROOT / "skills" / "deliver" / "templates" / "RUN.template.json").read_text()


def test_retired_change_identity_is_absent_and_readme_diagram_has_user_gates():
    assert not (ROOT / "skills" / "change").exists()
    readme = (ROOT / "README.md").read_text()
    assert "$change" not in readme
    # `deliver` is the kernel the whole lifecycle hangs off. Anchor on the claim,
    # not on a node label: labels are editorial, the one-run-one-receipt contract
    # is not.
    assert "the kernel binding one run to one receipt" in readme
    assert "[`deliver`](skills/deliver/SKILL.md)" in readme
    lifecycle = readme.split("## Lifecycle", 1)[1].split("## Core workflows", 1)[0]
    diagrams = re.findall(r"```mermaid\n(.*?)\n```", lifecycle, re.DOTALL)
    semantics = "\n".join(diagrams)
    assert len(diagrams) == 1
    assert all("accTitle:" in diagram and "accDescr:" in diagram for diagram in diagrams)
    # Mermaid quotes every drawn label (nodes and edges) and leaves accTitle and
    # accDescr unquoted, so this is what a sighted reader actually sees.
    drawn = "\n".join(re.findall(r'"([^"]*)"', semantics))
    assert drawn.count("USER ·") == 3
    for stage in ("scope", "implement", "verify", "review", "release", "observe", "retrospect"):
        assert stage in drawn, f"{stage} is not drawn in the lifecycle diagram"
    # A blocking finding must visibly return the work to implement, otherwise the
    # diagram shows review as advisory.
    assert "blocking finding" in drawn
    # `session` and `deliver` frame the loop without being stages in it, so they
    # are owed to a screen reader rather than drawn as nodes.
    description = re.search(r"accDescr:\s*(.+)", semantics).group(1)
    for framing in ("session", "deliver"):
        assert framing in description


@pytest.mark.skipif(shutil.which("mmdc") is None, reason="optional local Mermaid CLI is absent")
def test_readme_mermaid_parses_with_available_local_renderer(tmp_path):
    readme = (ROOT / "README.md").read_text()
    diagrams = re.findall(r"```mermaid\n(.*?)\n```", readme, re.DOTALL)
    for index, diagram in enumerate(diagrams):
        source = tmp_path / f"diagram-{index}.mmd"
        output = tmp_path / f"diagram-{index}.svg"
        source.write_text(diagram)
        subprocess.run(
            ["mmdc", "-i", str(source), "-o", str(output)],
            cwd=tmp_path,
            check=True,
            capture_output=True,
            text=True,
        )
        assert output.is_file()


def test_engineering_docs_requires_visual_diagram_qa():
    skill_root = ROOT / "skills" / "engineering-docs"
    skill = (skill_root / "SKILL.md").read_text()
    quality = (skill_root / "references" / "diagram-quality.md").read_text()
    assert "visually inspect" in skill
    assert "narrow widths" in skill
    assert '(cd "$out" && mmdc' in skill
    assert "one conceptual level per diagram" in quality
    assert "target's Mermaid version" in quality
    assert "temporary working directory" in quality
    assert "accTitle" in quality and "accDescr" in quality
    assert "Never draw a false transition" in quality


def test_readme_catalogue_contains_every_portable_skill():
    skills = {path.parent.name for path in (ROOT / "skills").glob("*/SKILL.md")}
    readme = (ROOT / "README.md").read_text()
    catalogue = readme.split("<!-- skill-catalogue:start -->", 1)[1].split(
        "<!-- skill-catalogue:end -->", 1
    )[0]
    listed = set(re.findall(r"`([a-z0-9-]+)`", catalogue))
    assert listed == skills
    for name in skills:
        assert f"(skills/{name}/SKILL.md)" in catalogue


def _catalogue_check(readme_text: str, tmp_path: Path) -> int:
    """Run the real gate over a README, and return its exit status."""
    readme = tmp_path / "README.md"
    readme.write_text(readme_text)
    return subprocess.run(
        [sys.executable, str(ROOT / "scripts" / "render_skill_catalogue.py"), "--check", "--readme", str(readme), "--skills-root", str(ROOT / "skills")],
        capture_output=True,
        text=True,
    ).returncode


def test_readme_headline_skill_count_matches_the_skills_on_disk(tmp_path):
    # Regression gate. The README claimed 34 skills while 33 shipped: the catalogue
    # table was guarded, the headline integer was not.
    assert _catalogue_check((ROOT / "README.md").read_text(), tmp_path) == 0


@pytest.mark.parametrize(
    ("name", "mutate"),
    (
        # The historical defect, verbatim: an unmanaged count beside the noun.
        ("the original count wording", lambda text: text.replace(f"<!--skills-->{SKILL_COUNT}<!--/skills--> Agent Skills", f"{SKILL_COUNT - 1} reusable Agent Skills")),
        ("a plain miscount", lambda text: text.replace(f"<!--skills-->{SKILL_COUNT}<!--/skills--> Agent Skills", f"{SKILL_COUNT - 2} Agent Skills")),
        # Whitespace must not smuggle a stale figure past the audit. Both of these render
        # in Markdown as the same false headline as the case above.
        ("a double space before the noun", lambda text: text.replace(f"<!--skills-->{SKILL_COUNT}<!--/skills--> Agent Skills", f"{SKILL_COUNT}  Agent Skills")),
        ("a line wrap before the noun", lambda text: text.replace(f"<!--skills-->{SKILL_COUNT}<!--/skills--> Agent Skills", f"{SKILL_COUNT}\nAgent Skills")),
        # Silence must not pass: a gate that only compares stated counts would go green
        # on a README that states none, which is drift by deletion.
        ("no count stated at all", lambda text: text.replace(f"<!--skills-->{SKILL_COUNT}<!--/skills--> Agent Skills", "Agent Skills").replace(f"<!--skills-->{SKILL_COUNT}<!--/skills-->-skill", "multi-skill")),
        ("a skill listed twice", lambda text: text.replace("[`session`](skills/session/SKILL.md), ", "[`session`](skills/session/SKILL.md), [`session`](skills/session/SKILL.md), ", 1)),
        ("a skill missing from the table", lambda text: text.replace("[`caveman`](skills/caveman/SKILL.md)", "")),
        ("a skill in the table that is not on disk", lambda text: text.replace("[`caveman`](skills/caveman/SKILL.md)", "[`caveman`](skills/caveman/SKILL.md), [`ghost`](skills/ghost/SKILL.md)")),
        ("a second catalogue block", lambda text: text + "\n<!-- skill-catalogue:start -->\n| Area | Skills |\n<!-- skill-catalogue:end -->\n"),
        # Marker deletion must not opt the README out of the drift gate: an absent
        # region is a loud failure, never a silent skip.
        ("the marked region deleted outright", lambda text: re.sub(r"<!-- skill-catalogue:start -->.*?<!-- skill-catalogue:end -->", "", text, flags=re.DOTALL)),
    ),
)
def test_catalogue_gate_rejects_every_known_way_the_count_can_rot(name, mutate, tmp_path):
    original = (ROOT / "README.md").read_text()
    mutated = mutate(original)
    assert mutated != original, f"the {name} mutation did not change the README, so it proves nothing"
    assert _catalogue_check(mutated, tmp_path) != 0, f"the catalogue gate accepted {name}"


def test_catalogue_gate_never_rewrites_a_number_it_does_not_own(tmp_path):
    # The gate must not become the thing it guards against. An earlier pattern matched
    # any digit near "skills", so it would have rewritten a truthful subset count
    # ("5 writing skills") into the library total, writing a false claim of its own.
    # The count is now marked, so an unowned number is refused, never silently changed.
    readme = tmp_path / "README.md"
    readme.write_text((ROOT / "README.md").read_text().replace(
        "## Core workflows", "The catalogue includes 5 writing skills.\n\n## Core workflows", 1
    ))
    before = readme.read_text()
    result = subprocess.run(
        [sys.executable, str(ROOT / "scripts" / "render_skill_catalogue.py"), "--readme", str(readme), "--skills-root", str(ROOT / "skills")],
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0, "an unowned skill count was accepted"
    assert "5 writing skills" in readme.read_text(), "the generator rewrote a number it does not own"
    assert readme.read_text() == before, "a failing run must not write"


def test_openai_skill_sidecar_descriptions_fit_provider_contract():
    for path in (ROOT / "skills").glob("*/agents/openai.yaml"):
        value = yaml.safe_load(path.read_text())
        description = value["interface"]["short_description"]
        assert 25 <= len(description) <= 64, path


def test_orchestrate_static_checker_does_not_claim_model_routing_passes():
    checker = ROOT / "skills" / "orchestrate" / "evals" / "check_skill_triggers.py"
    result = subprocess.run(
        [sys.executable, str(checker)],
        check=True,
        capture_output=True,
        text=True,
    )
    assert "SKILL DOCTRINE CHECK: PASS" in result.stdout
    assert "routing evidence is external" in result.stdout
    assert " explicit" not in result.stdout
    assert " inferred" not in result.stdout


@pytest.mark.parametrize("broken_suffix", ("js", "mjs", "cjs"))
def test_skill_javascript_gate_checks_all_module_suffixes_and_prunes_dependencies(
    tmp_path: Path,
    broken_suffix: str,
):
    checker = ROOT / "scripts" / "check-skill-javascript"
    assert checker.stat().st_mode & 0o111
    skills = tmp_path / "skills"
    skills.mkdir()
    (skills / "valid.js").write_text("const valid = true;\n")
    (skills / "valid.mjs").write_text(
        "export const valid = await Promise.resolve(true);\n"
    )
    (skills / "valid.cjs").write_text("module.exports = { valid: true };\n")
    vendored = skills / "node_modules"
    vendored.mkdir()
    (vendored / "ignored.mjs").write_text("const = broken;\n")

    passing = subprocess.run(
        [str(checker), str(skills)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert passing.returncode == 0, passing.stderr
    assert "PASS: checked 3 skill JavaScript files" in passing.stdout

    broken = skills / f"broken.{broken_suffix}"
    broken.write_text("const = broken;\n")
    failing = subprocess.run(
        [str(checker), str(skills)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert failing.returncode != 0
    assert str(broken) in failing.stderr


def test_skill_javascript_gate_rejects_an_empty_tree(tmp_path: Path):
    checker = ROOT / "scripts" / "check-skill-javascript"
    result = subprocess.run(
        [str(checker), str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0
    assert "no skill JavaScript files found" in result.stderr


@pytest.mark.parametrize(
    "content",
    (
        "schema_version: 1\ndoctrine_invariants: []\nreference_invariants: []\n",
        "schema_version: 1\ndoctrine_invariants:\n  - only one\n",
        "[]\n",
    ),
)
def test_orchestrate_doctrine_checker_rejects_empty_or_malformed_contracts(
    tmp_path, content
):
    cases = tmp_path / "contract_cases.yaml"
    cases.write_text(content)
    checker = ROOT / "skills" / "orchestrate" / "evals" / "check_skill_triggers.py"
    result = subprocess.run(
        [sys.executable, str(checker), "--cases", str(cases)],
        capture_output=True,
        text=True,
    )
    assert result.returncode == 1
    assert "SKILL CHECK: FAIL" in result.stdout


def test_readme_is_concise_public_facing_and_free_of_process_commentary():
    readme = (ROOT / "README.md").read_text()
    # The README is the front page: it earns a reader's next click, it is not the
    # manual. The cap keeps new sections competing for space with old ones instead
    # of accreting, and sends depth to docs/ARCHITECTURE.md.
    #
    # The cap needs real headroom or it stops being a sprawl guard and becomes a
    # freeze: the previous 1000 sat 5 words above a 995-word README, so every edit,
    # including correcting a false claim, had to pay for itself by deleting prose.
    # That is how disclosures get traded away for adjectives. If a change needs more
    # room than this, the README has stopped being a front page and the depth belongs
    # in docs/.
    #
    # Raised 1400 -> 1800 on 2026-07-21 (user decision): the front page now carries
    # two on-page Mermaid diagrams — the architecture overview and the delivery loop —
    # so a reviewer sees the shape without leaving the page. Their source and the
    # loop's mandatory screen-reader accDescr count toward the total, so the cap holds
    # headroom above them rather than forcing prose out to make room for a picture.
    assert len(readme.split()) <= 1800
    for retired_phrase in (
        "Experimental:",
        "made public for reuse",
        "stay out of the geometry",
        "The short constitution",
        "Skill maintainers should start",
        "Before publishing a fork",
    ):
        assert retired_phrase not in readme
    assert "scripts/install-harness" in readme
    assert "SECURITY.md" in readme
    installer = ROOT / "scripts" / "install-harness"
    assert installer.is_file()
    assert installer.stat().st_mode & 0o111


def test_react_performance_skill_is_vendor_neutral_lean_and_vite_aware():
    root = ROOT / "skills" / "react-performance"
    skill = (root / "SKILL.md").read_text()
    assert frontmatter_name(root / "SKILL.md") == "react-performance"
    assert count_skill_words(skill) <= 500
    assert "Vite" in skill
    assert not (ROOT / "skills" / "vercel-react-best-practices").exists()
    assert not any((root / name).exists() for name in ("AGENTS.md", "README.md", "metadata.json"))
    assert not list((root / "rules").glob("js-*.md"))
    assert (root / "references" / "vite.md").is_file()


def test_natural_writing_replaces_humanise_text_with_a_lean_general_fallback():
    root = ROOT / "skills" / "natural-writing"
    skill = (root / "SKILL.md").read_text()
    patterns = (root / "references" / "patterns.md").read_text()
    interface = (root / "agents" / "openai.yaml").read_text()
    tracked_text = "\n".join(
        path.read_text()
        for path in (ROOT / "README.md", ROOT / "HARNESS.md", ROOT / "MAINTAINING.md")
    )
    assert frontmatter_name(root / "SKILL.md") == "natural-writing"
    assert count_skill_words(skill) <= 500
    assert not (ROOT / "skills" / "humanise-text").exists()
    assert not (ROOT / "skills" / "clean-writing").exists()
    assert "humanise-text" not in tracked_text
    assert "clean-writing" not in tracked_text
    assert "engineering-writing" in skill
    assert "academic-writing" in skill
    assert "legal-writing" in skill
    assert "never proof of authorship" in patterns
    assert "2026.eacl-long.307" in patterns
    assert "2026.acl-long.2030" in patterns
    assert "full-humanise" not in skill + patterns
    assert "$natural-writing" in interface


def test_retrospect_closes_the_quality_flywheel_without_log_bloat():
    path = ROOT / "skills" / "retrospect" / "SKILL.md"
    skill = path.read_text()
    assert frontmatter_name(path) == "retrospect"
    assert count_skill_words(skill) <= 500
    for term in ("Benchmark", "Diagnose", "Verify", "Monitor"):
        assert term in skill
    assert "one dated log per run" in skill
    assert "proposal-first and read-only by default" in skill
    assert "user-approved scope" in skill


def test_current_docs_use_live_issue_and_durable_decision_owners():
    specs_index = (ROOT / "docs" / "specs" / "README.md").read_text()
    handoffs = (ROOT / "docs" / "handoffs" / "README.md").read_text()
    historical_specs = (
        ROOT / "docs" / "specs" / "harness" / "lifecycle.md",
        ROOT / "docs" / "specs" / "agent-fabric" / "activation.md",
        ROOT / "docs" / "specs" / "agent-fabric" / "provider-write-containment.md",
    )

    assert "issues/141" in specs_index
    # The handoffs index must keep its Active section and its durable-owner
    # routing, but must not pin the transient empty state: committing an active
    # handoff entry (docs custody, D15) is legitimate and must not fail this
    # contract. Assert structure, not the "No active handoffs." literal.
    assert "## Active" in handoffs
    assert "[GitHub Issues](https://github.com/mblauberg/provenant/issues)" in handoffs
    assert "Project Status owns workflow state" in handoffs
    assert not (ROOT / "docs" / "handoffs" / "consolidated-harness-cli.md").exists()
    assert (ROOT / "docs" / "adr" / "0013-thin-provenant-cli.md").is_file()
    for path in historical_specs:
        text = path.read_text()
        assert "owns current delivery" not in text
        assert "own delivery state" not in text


def test_current_architecture_and_install_bootstrap_use_user_terminology():
    architecture = (ROOT / "docs" / "ARCHITECTURE.md").read_text()
    installer = (ROOT / "scripts" / "install-harness").read_text()

    assert re.search(r"\bhuman(?:s)?\b|human-", architecture, re.IGNORECASE) is None
    assert "explicit user authority" in installer
    assert "explicit human authority" not in installer
