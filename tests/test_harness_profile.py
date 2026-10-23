"""Harness profiles: registry, resolution order, CLI and the attempt stamp."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace

import pytest

from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]
CLI = ROOT / "scripts/provenant-profile"


def load(name: str, path: Path):
    return load_script(path, name, register=True)


hp = load("harness_profile_under_test", ROOT / "skills/_shared/harness_profile.py")
REGISTRY = hp.load_registry(ROOT)


@pytest.fixture
def space(tmp_path):
    """An empty instance root and a project checkout, with no ambient profile variables."""
    instance, project = tmp_path / "instance", tmp_path / "project"
    (instance / "config").mkdir(parents=True)
    (project / ".git").mkdir(parents=True)
    environ = {key: value for key, value in os.environ.items()
               if not key.startswith(("PROVENANT_KNOB_", "PROVENANT_PROFILE", "PROVENANT_RISK_TIER"))}
    environ.update(AGENT_FABRIC_INSTANCE_ROOT=str(instance), AGENT_FABRIC_PRODUCT_ROOT=str(ROOT))
    return SimpleNamespace(instance=instance, project=project, environ=environ,
                           user=instance / "config/harness-profile.json",
                           project_file=project / ".agents/harness-profile.json")


def write(path: Path, document: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(document))


def resolved(space, **kwargs):
    return hp.current(cwd=space.project, environ=space.environ, registry=REGISTRY, **kwargs)


# Registry ------------------------------------------------------------------

def test_balanced_is_every_default_and_has_a_stable_digest(space):
    resolution = resolved(space)
    assert resolution.preset == "balanced"
    assert resolution.overrides == {}
    assert set(resolution.sources.values()) == {"default"}
    assert resolution.knobs == {name: spec["default"] for name, spec in REGISTRY.knobs.items()}
    assert resolution.digest == hp.resolve(REGISTRY, layers=[("env", {"preset": "balanced"})]).digest


def test_registry_entries_name_an_existing_reader_and_a_trade_off():
    wired = [name for name, spec in REGISTRY.knobs.items() if spec["status"] == "wired"]
    assert 4 <= len(wired) <= 8
    for name, spec in REGISTRY.knobs.items():
        reader = spec["reader"].split(" ", 1)[0].rstrip(",")
        assert (ROOT / reader).exists(), (name, reader)
        assert spec["trade_off"] and spec["effect"]


def test_presets_cover_the_three_postures_and_only_use_known_task_classes():
    assert {"economy", "balanced", "rigorous"} <= set(REGISTRY.presets)
    for preset in REGISTRY.presets.values():
        assert set(preset.get("task_classes", {})) <= REGISTRY.task_classes


def test_registry_rejects_a_knob_without_a_trade_off(tmp_path):
    for name in ("config/risk-policy.json", "config/model-routing.json"):
        (tmp_path / name).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(ROOT / name, tmp_path / name)
    document = json.loads((ROOT / hp.REGISTRY).read_text())
    del document["knobs"]["effort.shift"]["trade_off"]
    (tmp_path / hp.REGISTRY).write_text(json.dumps(document))
    with pytest.raises(hp.ProfileError, match="trade_off"):
        hp.load_registry(tmp_path)


def test_balanced_preset_may_not_change_anything(tmp_path):
    for name in ("config/risk-policy.json", "config/model-routing.json"):
        (tmp_path / name).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(ROOT / name, tmp_path / name)
    document = json.loads((ROOT / hp.REGISTRY).read_text())
    document["presets"]["balanced"]["knobs"] = {"lanes.max_concurrency": 4}
    (tmp_path / hp.REGISTRY).write_text(json.dumps(document))
    with pytest.raises(hp.ProfileError, match="balanced"):
        hp.load_registry(tmp_path)


# Resolution order ------------------------------------------------------------

def test_each_layer_overrides_the_one_below(space):
    write(space.user, {"preset": "economy", "knobs": {"lanes.max_concurrency": 3, "council.junior_size": 5}})
    write(space.project_file, {"knobs": {"lanes.max_concurrency": 2, "quota.pace_tolerance": 6}})
    space.environ["PROVENANT_KNOB_QUOTA_PACE_TOLERANCE"] = "7"
    resolution = resolved(space, dispatch={"council.junior_size": 1})
    assert resolution.preset == "economy"
    assert resolution.knobs["routing.pool_mode"] == "rotate"
    assert resolution.sources["routing.pool_mode"] == "preset:economy"
    assert (resolution.knobs["lanes.max_concurrency"], resolution.sources["lanes.max_concurrency"]) == (2, "project")
    assert (resolution.knobs["quota.pace_tolerance"], resolution.sources["quota.pace_tolerance"]) == (7, "env")
    assert (resolution.knobs["council.junior_size"], resolution.sources["council.junior_size"]) == (1, "dispatch")


def test_the_highest_layer_naming_a_preset_wins(space):
    write(space.user, {"preset": "economy"})
    write(space.project_file, {"preset": "rigorous"})
    assert resolved(space).preset == "rigorous"
    space.environ["PROVENANT_PROFILE"] = "balanced"
    resolution = resolved(space)
    assert resolution.preset == "balanced" and resolution.overrides == {}


def test_a_task_class_entry_beats_its_layer_but_not_a_higher_layer(space):
    write(space.user, {"knobs": {"council.junior_size": 2},
                       "task_classes": {"legwork": {"preset": "economy", "knobs": {"council.junior_size": 6}}}})
    legwork = resolved(space, task_class="legwork")
    assert legwork.preset == "economy"
    assert (legwork.knobs["council.junior_size"], legwork.sources["council.junior_size"]) == (6, "user (legwork)")
    assert resolved(space, task_class="review").knobs["council.junior_size"] == 2
    write(space.project_file, {"knobs": {"council.junior_size": 4}})
    assert resolved(space, task_class="legwork").knobs["council.junior_size"] == 4


def test_economy_preset_leaves_effort_at_default():
    economy = [("env", {"preset": "economy"})]
    assert hp.resolve(REGISTRY, layers=economy).knobs["effort.shift"] == 0
    rigorous = [("env", {"preset": "rigorous"})]
    assert hp.resolve(REGISTRY, task_class="review", layers=rigorous).knobs["effort.shift"] == 1
    assert hp.resolve(REGISTRY, task_class="mechanical", layers=rigorous).knobs["effort.shift"] == 0


def test_risk_tier_floor_raises_a_value_at_its_tier_and_above():
    economy = [("env", {"preset": "economy"})]
    assert hp.resolve(REGISTRY, risk_tier="substantial", layers=economy).knobs["effort.shift"] == 0
    for tier in ("crucial", "terminal"):
        resolution = hp.resolve(REGISTRY, risk_tier=tier,
                                layers=[*economy, ("dispatch", {"knobs": {"effort.shift": -1}})])
        assert resolution.knobs["effort.shift"] == 0
        assert resolution.sources["effort.shift"] == f"floor:{tier}"
        assert resolution.stamp()["floored"] == ["effort.shift"]


def test_environment_values_parse_by_type(space):
    space.environ.update({
        "PROVENANT_KNOB_CONTEXT_CEILING_TOKENS": "250000",
        "PROVENANT_KNOB_REVIEW_CROSS_FAMILY": "require",
        "PROVENANT_KNOB_REPAIR_MAX_CYCLES": "null",
        "PROVENANT_RISK_TIER": "crucial",
        "PROVENANT_KNOB_EFFORT_SHIFT": "-1",
    })
    resolution = resolved(space)
    assert resolution.knobs["context.ceiling_tokens"] == 250000
    assert resolution.knobs["review.cross_family"] == "require"
    assert resolution.knobs["repair.max_cycles"] is None
    assert resolution.risk_tier == "crucial" and resolution.knobs["effort.shift"] == 0


def test_digest_tracks_behaviour_not_how_it_was_reached(space):
    write(space.user, {"knobs": {"lanes.max_concurrency": 4}})
    via_file = resolved(space)
    via_env = hp.resolve(REGISTRY, layers=[("env", {"knobs": {"lanes.max_concurrency": 4}})])
    assert via_file.digest == via_env.digest
    assert via_file.digest != resolved(SimpleNamespace(**{**vars(space), "environ": {
        **space.environ, "PROVENANT_KNOB_LANES_MAX_CONCURRENCY": "3"}})).digest


# Validation ------------------------------------------------------------------

@pytest.mark.parametrize("document, message", [
    ({"knobs": {"no.such": 1}}, "unknown knob"),
    ({"knobs": {"lanes.max_concurrency": 9}}, "from 1 to 8"),
    ({"knobs": {"lanes.max_concurrency": True}}, "integer"),
    ({"knobs": {"review.cross_family": "never"}}, "one of prefer, require"),
    ({"preset": "lavish"}, "unknown preset"),
    ({"task_classes": {"gardening": {}}}, "unknown task class"),
    ({"task_classes": {"legwork": {"effort.shift": 1}}}, "only preset and knobs"),
    ({"knob": {}}, "unknown field"),
])
def test_profile_files_are_validated_against_the_registry(space, document, message):
    write(space.project_file, document)
    with pytest.raises(hp.ProfileError, match=message):
        resolved(space)


@pytest.mark.parametrize("document", [
    {"preset": []}, {"preset": {}}, {"task_classes": {"legwork": {"preset": 3}}},
    {"task_classes": []}, {"knobs": []}, [],
])
def test_malformed_profile_types_are_profile_errors_not_crashes(space, document):
    write(space.project_file, document)
    with pytest.raises(hp.ProfileError):
        resolved(space)
    with pytest.raises(hp.ProfileError):
        hp.edit_file(REGISTRY, space.project_file, lambda doc: None)


def test_project_file_may_strengthen_but_not_weaken_review_knobs(space):
    write(space.user, {"preset": "rigorous"})
    write(space.project_file, {"preset": "economy", "knobs": {"review.cross_family": "prefer"}})
    resolution = resolved(space)
    assert resolution.preset == "economy"
    assert resolution.knobs["lanes.max_concurrency"] == 4  # other knobs follow the project
    assert resolution.knobs["review.extra_reviewers"] == 1
    assert resolution.knobs["review.cross_family"] == "require"
    assert resolution.knobs["review.rereview_scope"] == "full"
    assert resolution.sources["review.cross_family"] == "preset:rigorous (project may only strengthen)"

    write(space.user, {})
    write(space.project_file, {"knobs": {"review.rereview_scope": "full", "review.extra_reviewers": 2}})
    strengthened = resolved(space)
    assert strengthened.knobs["review.rereview_scope"] == "full"
    assert strengthened.knobs["review.extra_reviewers"] == 2

    # The user's own environment still decides above the project file.
    write(space.user, {"preset": "rigorous"})
    space.environ["PROVENANT_KNOB_REVIEW_EXTRA_REVIEWERS"] = "0"
    assert resolved(space).knobs["review.extra_reviewers"] == 0


def test_review_knobs_are_the_strengthen_only_ones():
    guarded = {name for name, spec in REGISTRY.knobs.items() if spec.get("project") == "strengthen"}
    assert guarded == {name for name in REGISTRY.knobs if name.startswith("review.")}


def test_unknown_knob_variables_and_tiers_are_errors(space):
    with pytest.raises(hp.ProfileError, match="PROVENANT_KNOB_NO_SUCH"):
        hp.current(cwd=space.project, environ={**space.environ, "PROVENANT_KNOB_NO_SUCH": "1"}, registry=REGISTRY)
    with pytest.raises(hp.ProfileError, match="risk tier"):
        resolved(space, risk_tier="severe")
    with pytest.raises(hp.ProfileError, match="NAME=VALUE"):
        hp.parse_assignments(REGISTRY, ["lanes.max_concurrency"])


def test_runtime_readers_fall_back_to_defaults_on_a_broken_profile(space, monkeypatch):
    write(space.project_file, {"knobs": {"lanes.max_concurrency": 99}})
    resolution, error = hp.lenient(cwd=space.project, environ=space.environ, registry=REGISTRY)
    assert resolution is None and "lanes.max_concurrency" in error
    assert hp.value("lanes.max_concurrency", 8, cwd=space.project, environ=space.environ, registry=REGISTRY) == 8


# CLI -------------------------------------------------------------------------

def cli(space, *arguments: str):
    return subprocess.run([str(CLI), *arguments], cwd=space.project, env=space.environ,
                          capture_output=True, text=True, timeout=60)


def test_cli_use_set_reset_show_round_trip(space):
    assert cli(space, "use", "economy").returncode == 0
    assert json.loads(space.user.read_text())["preset"] == "economy"
    assert cli(space, "set", "lanes.max_concurrency=2", "review.cross_family=require", "--project").returncode == 0
    assert cli(space, "set", "effort.shift=1", "--project", "--task-class", "legwork").returncode == 0
    project = json.loads(space.project_file.read_text())
    assert project["knobs"] == {"lanes.max_concurrency": 2, "review.cross_family": "require"}
    assert project["task_classes"] == {"legwork": {"knobs": {"effort.shift": 1}}}

    shown = cli(space, "show", "--resolved", "--task-class", "legwork", "--json")
    stamp = json.loads(shown.stdout)
    assert stamp["preset"] == "economy" and stamp["knobs"]["effort.shift"] == 1
    assert stamp["sources"]["lanes.max_concurrency"] == "project"

    assert cli(space, "reset", "lanes.max_concurrency", "review.cross_family", "--project").returncode == 0
    assert cli(space, "reset", "effort.shift", "--project", "--task-class", "legwork").returncode == 0
    assert json.loads(space.project_file.read_text()) == {"schema_version": 1}
    assert cli(space, "reset", "preset").returncode == 0
    assert "preset balanced" in cli(space, "show", "--resolved").stdout


def test_cli_rejects_unknown_knobs_values_and_presets_without_writing(space):
    for arguments, message in ((("set", "no.such=1"), "unknown knob"),
                               (("set", "lanes.max_concurrency=0"), "from 1 to 8"),
                               (("use", "lavish"), "unknown preset"),
                               (("show", "--resolved", "--task-class", "gardening"), "unknown task class")):
        result = cli(space, *arguments)
        assert result.returncode == 2 and message in result.stderr, arguments
    assert not space.user.exists()


def test_cli_list_and_diff(space):
    listing = cli(space, "list")
    assert listing.returncode == 0
    assert "effort.shift  [wired]" in listing.stdout and "rigorous:" in listing.stdout
    diff = json.loads(cli(space, "diff", "balanced", "rigorous", "--task-class", "mechanical", "--json").stdout)
    assert "effort.shift" not in diff["differs"]
    assert diff["differs"]["review.rereview_scope"] == ["delta", "full"]
    assert diff["left"]["digest"] == resolved(space).digest


# Wired readers ---------------------------------------------------------------

def dispatch_args(**extra):
    return SimpleNamespace(**{
        "tool": "codex", "alias": "senior", "model": None, "alias_supplied": True, "effort": None,
        "task_id": "dispatch-001", "access_mode": "read_only", "worktree": None, "_phase_timings": {},
        "reviewer_id": None, "risk_tier": None, "model_override_tier": None, **extra})


def test_attempt_receipt_carries_the_profile_stamp(space, monkeypatch):
    dispatch = load("dispatch_run_profile_test", ROOT / "skills/orchestrate/scripts/dispatch_run.py")
    monkeypatch.chdir(space.project)
    for key in [key for key in os.environ if key.startswith(("PROVENANT_KNOB_", "PROVENANT_PROFILE", "PROVENANT_RISK"))]:
        monkeypatch.delenv(key)
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(space.instance))
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))
    # export_knob_overrides writes os.environ; registering the names restores them afterwards.
    monkeypatch.setenv("PROVENANT_KNOB_LANES_MAX_CONCURRENCY", "8")
    monkeypatch.setenv("PROVENANT_KNOB_EFFORT_SHIFT", "0")
    monkeypatch.setenv("PROVENANT_KNOB_LANES_SILENCE_SECONDS", "300")
    monkeypatch.setenv("PROVENANT_RISK_TIER", "routine")
    monkeypatch.delenv("PROVENANT_KNOB_LANES_MAX_CONCURRENCY")
    monkeypatch.delenv("PROVENANT_KNOB_EFFORT_SHIFT")
    monkeypatch.delenv("PROVENANT_KNOB_LANES_SILENCE_SECONDS")
    monkeypatch.delenv("PROVENANT_RISK_TIER")

    row = dispatch.contract_row(dispatch_args(), space.project, 1, space.project / "attempt", {}, "now")
    assert row["profile"]["preset"] == "balanced"
    assert row["profile"]["knobs"]["lanes.silence_seconds"] == 300
    assert row["profile"]["digest"] == resolved(space).digest

    write(space.user, {"preset": "economy"})
    args = dispatch_args(task_class="review", risk_tier="crucial",
                         knobs=["lanes.max_concurrency=3", "effort.shift=-1", "lanes.silence_seconds=120"])
    dispatch.dispatch_preflight.export_knob_overrides(args)
    assert os.environ["PROVENANT_KNOB_LANES_MAX_CONCURRENCY"] == "3"
    assert os.environ["PROVENANT_RISK_TIER"] == "crucial"
    stamp = dispatch.contract_row(args, space.project, 1, space.project / "attempt", {}, "now")["profile"]
    assert stamp["preset"] == "economy" and stamp["task_class"] == "review"
    assert stamp["knobs"]["lanes.max_concurrency"] == 3 and stamp["knobs"]["effort.shift"] == 0
    assert stamp["floored"] == ["effort.shift"]
    assert dispatch.dispatch_preflight.profile_knob(args, "context.ceiling_tokens") == 200000
    assert stamp["knobs"]["lanes.silence_seconds"] == 120
    assert os.environ["PROVENANT_KNOB_LANES_SILENCE_SECONDS"] == "120"

    with pytest.raises(dispatch.harness_profile.ProfileError, match="unknown knob"):
        dispatch.dispatch_preflight.export_knob_overrides(dispatch_args(knobs=["no.such=1"]))


def test_attempt_stamp_records_a_broken_profile_instead_of_failing(space, monkeypatch):
    dispatch = load("dispatch_run_profile_broken", ROOT / "skills/orchestrate/scripts/dispatch_run.py")
    monkeypatch.chdir(space.project)
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(space.instance))
    write(space.project_file, {"knobs": {"no.such": 1}})
    stamp = dispatch.contract_row(dispatch_args(), space.project, 1, space.project / "attempt", {}, "now")["profile"]
    assert stamp["preset"] is None and "no.such" in stamp["error"]


@pytest.mark.parametrize("profile_root", [None, ""])
def test_routing_reads_the_same_user_profile_as_the_receipt(space, monkeypatch, profile_root):
    """Routing may fall back to the product catalogue's instance root; the profile must not move with it."""
    dispatch = load("dispatch_run_profile_root", ROOT / "skills/orchestrate/scripts/dispatch_run.py")
    monkeypatch.chdir(space.project)
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(space.instance))  # no config/model-routing.json here
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))
    if profile_root is None:
        monkeypatch.delenv(hp.PROFILE_ROOT_ENV, raising=False)
    else:
        monkeypatch.setenv(hp.PROFILE_ROOT_ENV, profile_root)
    write(space.user, {"knobs": {"effort.shift": 1}})
    environment = dispatch.dispatch_preflight.routing_environment()
    assert environment["AGENT_FABRIC_INSTANCE_ROOT"] == str(ROOT)
    assert environment[hp.PROFILE_ROOT_ENV] == str(space.instance)
    assert hp.user_path(environment) == space.user == hp.user_path(os.environ)
    routed = route(space, None, "--task-class", "legwork", **{
        key: environment[key] for key in ("AGENT_FABRIC_INSTANCE_ROOT", hp.PROFILE_ROOT_ENV)})
    stamp = dispatch.contract_row(dispatch_args(task_class="legwork"), space.project, 1,
                                  space.project / "attempt", {}, "now")["profile"]
    assert routed["effort"] == "high" and stamp["knobs"]["effort.shift"] == 1


def test_batch_concurrency_is_capped_by_the_profile(space, monkeypatch):
    batch = load("batch_run_profile_test", ROOT / "skills/orchestrate/scripts/batch_run.py")
    monkeypatch.chdir(space.project)
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(space.instance))
    monkeypatch.setenv("PROVENANT_KNOB_LANES_MAX_CONCURRENCY", "2")
    monkeypatch.setattr(batch, "_validate_run_dir", lambda path: path)
    monkeypatch.setattr(batch, "_load_manifest", lambda *a: ([{}] * 5, None, b""))
    monkeypatch.setattr(batch, "DISPATCH_RUN", space.project / "missing-owner")
    args = SimpleNamespace(concurrency=4, run_dir=space.project, manifest=space.project / "m.json")
    assert batch.batch(args) == 2  # the missing owner stops it after the cap is applied
    assert args.concurrency == 2
    monkeypatch.delenv("PROVENANT_KNOB_LANES_MAX_CONCURRENCY")
    args = SimpleNamespace(concurrency=4, run_dir=space.project, manifest=space.project / "m.json")
    batch.batch(args)
    assert args.concurrency == 4


ROUTE = ROOT / "scripts/model_route.py"


def route(space, catalog: Path | None, *arguments: str, **environ: str):
    caps = space.project / "caps.json"
    caps.write_text(json.dumps({
        "schema_version": 1, "source": "codex debug models",
        "observed_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "models": {"gpt-6.1-sol": {"resolved_model": "gpt-6.1-sol",
                                   "supported_efforts": ["low", "medium", "high", "xhigh"]}}}))
    command = [sys.executable, str(ROUTE), "resolve", "--adapter", "codex", "--role", "worker",
               "--capabilities-file", str(caps), *arguments]
    if catalog:
        command += ["--catalog", str(catalog)]
    result = subprocess.run(command, cwd=space.project, capture_output=True, text=True, timeout=60,
                            env={**space.environ, "PATH": "", **environ})
    return json.loads(result.stdout)


def test_effort_shift_steps_a_task_class_route(space):
    assert route(space, None, "--task-class", "legwork")["effort"] == "medium"
    raised = route(space, None, "--task-class", "legwork", PROVENANT_KNOB_EFFORT_SHIFT="1")
    assert (raised["status"], raised["effort"], raised["effort_shift"]) == ("ok", "high", 1)
    # legwork already sits at its policy minimum, so economy cannot lower it.
    lowered = route(space, None, "--task-class", "legwork", PROVENANT_KNOB_EFFORT_SHIFT="-1")
    assert lowered["effort"] == "medium" and "effort_shift" not in lowered


def test_effort_shift_lowers_above_the_minimum_unless_the_tier_floors_it(space):
    catalog = json.loads((ROOT / "config/model-routing.json").read_text())
    catalog["task_class_routes"]["research"].update({
        "alias": "senior", "effort": "high", "role": "worker", "models": {"codex": ["gpt-6.1-sol"]}})
    path = space.project / "catalog.json"
    path.write_text(json.dumps(catalog))
    assert route(space, path, "--task-class", "research", PROVENANT_PROFILE="economy")["effort"] == "high"
    lowered = route(space, path, "--task-class", "research", PROVENANT_KNOB_EFFORT_SHIFT="-1")
    assert lowered["effort"] == "medium"
    floored = route(space, path, "--task-class", "research", PROVENANT_KNOB_EFFORT_SHIFT="-1", PROVENANT_RISK_TIER="crucial")
    assert floored["effort"] == "high"
