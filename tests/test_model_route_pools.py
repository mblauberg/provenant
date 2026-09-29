"""Global weighted route pools and their four selection modes (#848)."""

from collections import Counter
from datetime import datetime, timedelta, timezone
import importlib.util
import json
import os
from pathlib import Path
import random
import subprocess
import sys

import pytest


ROOT = Path(__file__).resolve().parents[1]
CATALOG = json.loads((ROOT / "config" / "model-routing.json").read_text())


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


router = load("pools_router_under_test", ROOT / "scripts" / "model_route.py")
pools = router._pools


def availability(catalog=CATALOG, installed=lambda adapter: True, cooling=()):
    until = (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat()
    checked = pools.Availability(router, catalog, installed=installed,
                                 cooldowns={key: {"cooling_until": until} for key in cooling})
    checked._enabled = {adapter: "" for adapter in catalog["adapters"]}  # skip per-test adapter YAML parsing
    return checked


def pick(request, tmp_path, catalog=CATALOG, seed=0, **options):
    return pools.pick(router, catalog, {"project": "/work/a", **request},
                      availability=options.pop("available", None) or availability(catalog, **options),
                      state_path=tmp_path / "route-rotation.json", effort_order=router.EFFORT_ORDER,
                      rng=random.Random(seed))


def models(result):
    return [f"{item['adapter']}/{item['model']}" for item in result["picks"]]


def test_seed_routes_name_registered_models_with_the_tiny_weight_vocabulary():
    assert set(CATALOG["routes"]) == {"strong", "bulk", "design", "writing"}
    for name, entries in CATALOG["routes"].items():
        for entry in entries:
            assert entry["weight"] in {"high", "normal", "sparing"}
            adapter, model = pools.split_model(router, CATALOG, entry["model"])
            registered, _ = router._registered_match(adapter, model, CATALOG)
            assert registered is not None, f"{name}: {entry['model']}"
            for effort in entry.get("effort", []):
                assert effort in registered["efforts"]
    assert all(target in CATALOG["routes"] for target in CATALOG["route_synonyms"].values())
    assert set(CATALOG["task_class_routes"]) <= set(CATALOG["route_synonyms"]) | set(CATALOG["routes"])


def test_top_is_the_highest_weight_available_and_deterministic(tmp_path):
    first = pick({"route": "strong"}, tmp_path, seed=1)
    assert models(first) == ["claude/claude-opus-5-5"]
    assert first["picks"][0]["reason"] == "strong top"
    assert models(pick({"route": "strong"}, tmp_path, seed=99)) == models(first)
    assert models(pick({"route": "bulk"}, tmp_path)) == ["codex/gpt-6-luna"]


def test_every_mode_filters_cooling_uninstalled_and_disabled_models(tmp_path):
    cooling = pick({"route": "strong"}, tmp_path, cooling=("claude/claude-opus-5-5",))
    assert models(cooling) == ["codex/gpt-6.1-sol"]
    assert cooling["picks"][0]["effort"] == "high"
    assert any("claude-opus-5-5: cooling until" in warning for warning in cooling["warnings"])
    missing = pick({"route": "strong", "council": 3}, tmp_path, installed=lambda adapter: adapter != "claude")
    assert "claude/claude-opus-5-5" not in models(missing)
    assert any("not installed" in warning for warning in missing["warnings"])
    with pytest.raises(pools.PoolError) as error:
        pick({"route": "writing"}, tmp_path, installed=lambda adapter: False)
    assert error.value.code == "route_unavailable"
    disabled = availability()
    disabled._enabled["claude"] = "disabled"
    assert models(pick({"route": "strong"}, tmp_path, available=disabled)) == ["codex/gpt-6.1-sol"]


def test_route_effort_band_clamps_the_callers_effort(tmp_path):
    cooling = ("claude/claude-opus-5-5",)
    assert pick({"route": "strong", "effort": "low"}, tmp_path, cooling=cooling)["picks"][0]["effort"] == "high"
    assert pick({"route": "strong", "effort": "max"}, tmp_path, cooling=cooling)["picks"][0]["effort"] == "xhigh"
    assert pick({"route": "strong", "effort": "xhigh"}, tmp_path, cooling=cooling)["picks"][0]["effort"] == "xhigh"
    assert pick({"route": "strong", "effort": "medium"}, tmp_path)["picks"][0]["effort"] == "medium"


def test_council_spreads_families_first_and_names_each_member(tmp_path):
    for seed in range(40):
        result = pick({"route": "design", "council": 3}, tmp_path, seed=seed)
        families = [item["family"] for item in result["picks"]]
        assert len(set(families)) == 3, (seed, families)
        assert [item["reason"] for item in result["picks"]] == [
            "design council 1/3", "design council 2/3", "design council 3/3"]
        strong = pick({"route": "strong", "council": 2}, tmp_path, seed=seed)
        assert {item["family"] for item in strong["picks"]} == {"anthropic", "openai"}


def test_council_weights_are_sampling_odds_and_sparing_is_rare(tmp_path):
    drawn = Counter(models(pick({"route": "strong", "council": 1}, tmp_path, seed=seed))[0] for seed in range(400))
    assert drawn["claude/claude-opus-5-5"] > drawn["codex/gpt-6.1-sol"] > drawn["codex/gpt-6-astra"] > 0
    assert drawn["codex/gpt-6-astra"] < 400 / 8


def test_council_larger_than_the_pool_repeats_with_a_warning(tmp_path):
    result = pick({"route": "strong", "council": 5}, tmp_path)
    assert len(result["picks"]) == 5
    assert any("exceeds 3 available" in warning for warning in result["warnings"])


def test_confidential_skips_free_training_models_and_otherwise_warns(tmp_path):
    free = "opencode/opencode/muse-spark-1.3-contributor-free"
    open_council = [pick({"route": "design", "council": 4}, tmp_path, seed=seed) for seed in range(20)]
    chosen = [result for result in open_council if free in models(result)]
    assert chosen, "free models stay in an ordinary pool"
    assert any("may train on prompts" in warning and "confidential" in warning for warning in chosen[0]["warnings"])
    for seed in range(20):
        private = pick({"route": "design", "council": 3, "confidential": True}, tmp_path, seed=seed)
        assert not any(model.endswith("-free") for model in models(private))
        assert any(f"skipped {free}: trains on prompts" in warning for warning in private["warnings"])
    with pytest.raises(pools.PoolError) as error:
        pick({"models": ["opencode/mimo-free"], "confidential": True}, tmp_path)
    assert error.value.code == "route_unavailable"


def test_rotate_cycles_by_weight_with_a_per_project_cursor(tmp_path):
    sequence = [models(pick({"route": "strong", "rotate": True}, tmp_path))[0] for _ in range(13)]
    assert Counter(sequence) == {"claude/claude-opus-5-5": 8, "codex/gpt-6.1-sol": 4, "codex/gpt-6-astra": 1}
    assert len(set(sequence[:2])) == 2, "rotation mixes models from the second call"
    assert pick({"route": "strong", "rotate": True}, tmp_path)["picks"][0]["reason"] == "strong rotate"
    other = [models(pick({"route": "strong", "rotate": True, "project": "/work/b"}, tmp_path))[0] for _ in range(2)]
    assert other == sequence[:2]
    state = json.loads((tmp_path / "route-rotation.json").read_text())
    assert set(state["projects"]) == {"/work/a", "/work/b"}


def test_explicit_models_bypass_routes_and_keep_their_effort(tmp_path):
    result = pick({"models": ["codex/gpt-6-luna@low", "claude/haiku"]}, tmp_path)
    assert models(result) == ["codex/gpt-6-luna", "claude/haiku"]
    assert result["picks"][0]["effort"] == "low" and "effort" not in result["picks"][1]
    assert [item["reason"] for item in result["picks"]] == ["ad-hoc council 1/2", "ad-hoc council 2/2"]
    assert models(pick({"models": ["opencode/mimo-v2.6-flash-free"]}, tmp_path)) == [
        "opencode/opencode/mimo-v2.6-flash-free"]
    with pytest.raises(pools.PoolError) as error:
        pick({"models": ["codex/gpt-6-luna"], "route": "bulk"}, tmp_path)
    assert error.value.code == "route_conflict"


def test_task_classes_and_aliases_map_onto_routes(tmp_path):
    assert pick({"route": "review"}, tmp_path)["route"] == "strong"
    assert pick({"route": "Flagship"}, tmp_path)["route"] == "strong"
    assert pick({"route": "ui-taste"}, tmp_path)["route"] == "design"
    assert pick({"route": "scout"}, tmp_path)["route"] == "bulk"
    with pytest.raises(pools.PoolError) as error:
        pick({"route": "fastest"}, tmp_path)
    assert error.value.code == "route_invalid"


def test_adapter_narrows_the_pool(tmp_path):
    assert models(pick({"route": "strong", "adapter": "codex"}, tmp_path)) == ["codex/gpt-6.1-sol"]
    with pytest.raises(pools.PoolError):
        pick({"route": "strong", "adapter": "kiro"}, tmp_path)


def overlay_environment(tmp_path, overlay):
    instance = tmp_path / "instance"
    (instance / "config").mkdir(parents=True)
    (instance / "config" / "model-routing.json").write_text(json.dumps(overlay))
    return {**os.environ, "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT), "AGENT_FABRIC_INSTANCE_ROOT": str(instance),
            "AGENT_FABRIC_STATE_ROOT": str(tmp_path / "state")}


def model_route(env, *args, stdin=None):
    return subprocess.run([sys.executable, str(ROOT / "scripts" / "model_route.py"), *args],
                          input=stdin, text=True, capture_output=True, env=env, check=False)


def test_instance_overlay_reorders_and_reweights_a_pool(tmp_path):
    env = overlay_environment(tmp_path, {"routes": {
        "strong": [{"model": "codex/gpt-6-astra", "weight": "high"},
                   {"model": "claude/claude-opus-5-5", "weight": "off"}],
        "local": [{"model": "codex/gpt-6-luna"}],
        "broken": [{"model": "no-adapter", "weight": "loud"}],
    }, "route_synonyms": {"deep": "strong"}})
    run = model_route(env, "routes", "--json")
    assert run.returncode == 0, run.stderr
    document = json.loads(run.stdout)
    strong = document["routes"]["strong"]
    assert [entry["model"] for entry in strong] == [
        "codex/gpt-6-astra", "claude/claude-opus-5-5", "codex/gpt-6.1-sol"]
    assert strong[1]["weight"] == "off" and strong[1]["availability"] == "off"
    assert strong[2]["effort"] == "high-xhigh", "unlisted entries keep their fields"
    assert document["routes"]["local"][0]["weight"] == "normal"
    assert "broken" not in document["routes"]
    request = json.dumps({"requests": [{"route": "deep"}, {"route": "local", "adapter": "codex"}]})
    picked = model_route(env, "pick", stdin=request)
    assert picked.returncode == 0, picked.stderr
    results = json.loads(picked.stdout)["results"]
    if results[0]["status"] == "ok":  # needs an installed codex CLI
        assert results[0]["picks"][0]["model"] == "gpt-6-astra"
    snapshot = json.loads(model_route(env, "snapshot", "--json").stdout)
    assert any(note.startswith("routes.broken") for note in snapshot["drift"])


def test_pick_command_answers_every_request_and_types_rejections(tmp_path):
    env = overlay_environment(tmp_path, {})
    run = model_route(env, "pick", "--seed", "3", stdin=json.dumps({"requests": [
        {"route": "nope"}, {"route": "strong", "council": 9}, "bad",
        {"models": ["codex/gpt-6-luna@warp"]},
    ]}))
    assert run.returncode == 0, run.stderr
    assert [result["error"] for result in json.loads(run.stdout)["results"]] == [
        "route_invalid", "council_invalid", "invalid_input", "effort_invalid"]


def test_routes_prints_each_pool_with_live_availability(tmp_path):
    env = overlay_environment(tmp_path, {})
    env["PATH"] = str(tmp_path / "empty-bin")
    state = tmp_path / "state"
    state.mkdir()
    until = (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat()
    (state / "cooldowns.json").write_text(json.dumps({"cooldowns": {"codex/gpt-6-luna": {"cooling_until": until}}}))
    run = model_route(env, "routes")
    assert run.returncode == 0, run.stderr
    for name in ("strong", "bulk", "design", "writing"):
        assert f"\n{name}\n" in f"\n{run.stdout}"
    assert "not installed" in run.stdout
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    (bin_dir / "codex").write_text("#!/bin/sh\n")
    (bin_dir / "codex").chmod(0o755)
    env["PATH"] = str(bin_dir)
    lines = model_route(env, "routes").stdout.splitlines()
    assert any("codex/gpt-6-luna" in line and "cooling until" in line for line in lines)
    assert any("codex/gpt-6-astra" in line and line.rstrip().endswith("available") for line in lines)
    assert "[free, trains-on-prompts]" in run.stdout
    provenant = subprocess.run([sys.executable, str(ROOT / "scripts" / "provenant"), "routes", "--json"],
                               text=True, capture_output=True, check=False,
                               env={**env, "PATH": os.environ["PATH"]})
    assert provenant.returncode == 0, provenant.stderr
    assert set(json.loads(provenant.stdout)["routes"]) >= {"strong", "bulk", "design", "writing"}


def test_route_line_names_why_the_pool_picked_the_model():
    sys.path.insert(0, str(ROOT / "skills" / "orchestrate" / "scripts"))
    dispatch_run = load("pools_dispatch_run_under_test", ROOT / "skills/orchestrate/scripts/dispatch_run.py")
    line = "Route: opencode/deepseek-v4.1-flash (deepseek; observed)"
    assert dispatch_run.with_pick_reason(line, "design council 2/3") == (
        "Route: opencode/deepseek-v4.1-flash (deepseek; observed; design council 2/3)")
    assert dispatch_run.with_pick_reason(line, None) == line
    once = dispatch_run.with_pick_reason(line, "design council 2/3")
    assert dispatch_run.with_pick_reason(once, "design council 2/3") == once
    args = dispatch_run.parser().parse_args([
        "--run-dir", "/tmp/run", "--prompt-file", "p.md", "--adapter", "codex", "--model", "gpt-6-luna",
        "--pick-reason", "bulk top"])
    args.task_id, args.access_mode, args.worktree = "task-1", "read_only", None
    row = dispatch_run.contract_row(args, Path("/tmp/run"), 1, Path("/tmp/run/a"),
                                    {"model": "gpt-6-luna", "route": {"model_family": "openai"}}, "now")
    assert row["provenance"]["line"] == "Route: codex/gpt-6-luna (openai; resolved; bulk top)"
    assert row["provenance"]["pick_reason"] == "bulk top"
    batch_run = load("pools_batch_run_under_test", ROOT / "skills/orchestrate/scripts/batch_run.py")
    command = batch_run._command({"id": "c-1", "adapter": "codex", "prompt_file": "p.md", "role": "worker",
                                  "timeout": 60, "model": "gpt-6-luna", "pick_reason": "ad-hoc council 1/2"},
                                 Path("/tmp/run"))
    assert command[command.index("--pick-reason") + 1] == "ad-hoc council 1/2"


def test_refresh_routing_carries_new_route_keys_and_keeps_instance_reweighting(tmp_path):
    sys.path.insert(0, str(ROOT / "scripts"))
    script = ROOT / "scripts" / "instance_installation.py"
    product = tmp_path / "product"
    (product / "config").mkdir(parents=True)
    (product / "package.json").write_text(json.dumps({"version": "1.2.3"}) + "\n")
    (product / "AGENTS.md").write_text("# Product doctrine\n")
    (product / "config" / "model-preferences.json").write_text("{}\n")
    source = product / "config" / "model-routing.json"
    source.write_text(json.dumps({"catalog_date": "2026-09-23", "adapters": {}}))
    instance_root = tmp_path / "instance"

    def run(action):
        result = subprocess.run([sys.executable, str(script), action, "--product-root", str(product),
                                 "--instance-root", str(instance_root)], text=True, capture_output=True)
        assert result.returncode == 0, result.stderr
        return result

    run("seed")
    run("refresh-routing")
    target = instance_root / "config" / "model-routing.json"
    source.write_text(json.dumps({"catalog_date": "2026-09-23", "adapters": {},
                                  "routes": CATALOG["routes"], "model_traits": CATALOG["model_traits"],
                                  "route_synonyms": CATALOG["route_synonyms"]}))
    run("refresh-routing")
    installed = json.loads(target.read_text())
    assert installed["routes"] == CATALOG["routes"]
    installed["routes"]["design"] = [{"model": "claude/claude-opus-5-5", "weight": "high"}]
    target.write_text(json.dumps(installed))
    changed = json.loads(source.read_text())
    changed["routes"]["writing"] = [{"model": "agy/gemini-3.8-flash", "weight": "high"}]
    source.write_text(json.dumps(changed))
    run("refresh-routing")
    refreshed = json.loads(target.read_text())
    assert refreshed["routes"]["design"] == [{"model": "claude/claude-opus-5-5", "weight": "high"}]
    assert refreshed["routes"]["writing"] == [{"model": "agy/gemini-3.8-flash", "weight": "high"}]
    assert refreshed["model_traits"] == CATALOG["model_traits"]

