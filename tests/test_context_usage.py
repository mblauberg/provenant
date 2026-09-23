"""Context-size capture, ceilings and resume advice, from recorded provider streams."""

import importlib
import json
from pathlib import Path
import sys

import pytest

ROOT = Path(__file__).resolve().parents[1]
FIX = ROOT / "tests/fixtures/fabric-context"


def context():
    return importlib.import_module("skills.orchestrate.scripts.context_usage")


def supervisor():
    return importlib.import_module("skills.orchestrate.scripts.provider_exec")


def measured(adapter, name=None):
    meter = context().Meter(adapter)
    for line in (FIX / f"{name or adapter}.jsonl").read_text().splitlines():
        meter.observe(json.loads(line))
    return meter.result()


# Values come from one live tiny turn per adapter (2026-09-23), redacted.
@pytest.mark.parametrize(
    "adapter,expected",
    [
        # Plan mode: init says haiku, claude-sonnet-5 (1M) answers.
        ("claude", {"context_tokens": 8039, "input_tokens": 8035, "output_tokens": 4,
                    "cached_input_tokens": 4587, "context_window_tokens": 1000000, "source": "observed"}),
        # acceptEdits: haiku answers with its 200k window.
        ("claude-direct-haiku", {"context_tokens": 4977, "input_tokens": 4934, "output_tokens": 43,
                                 "cached_input_tokens": 4098, "context_window_tokens": 200000,
                                 "source": "observed"}),
        ("codex", {"context_tokens": 16986, "input_tokens": 16981, "output_tokens": 5,
                   "cached_input_tokens": 11008, "context_window_tokens": None, "source": "estimated"}),
        ("cursor", {"context_tokens": 19124, "input_tokens": 19098, "output_tokens": 26,
                    "cached_input_tokens": 2176, "context_window_tokens": None, "source": "estimated"}),
        ("opencode", {"context_tokens": 17230, "input_tokens": 17228, "output_tokens": 2,
                      "cached_input_tokens": 1664, "context_window_tokens": None, "source": "observed"}),
        ("agy", {"context_tokens": 15400, "input_tokens": 15389, "output_tokens": 11,
                 "cached_input_tokens": 0, "context_window_tokens": None, "source": "observed"}),
        ("kiro", {"context_tokens": None, "input_tokens": None, "output_tokens": None,
                  "cached_input_tokens": None, "context_window_tokens": None, "source": "observed",
                  "context_percent": 7.7}),
    ],
)
def test_live_samples_yield_context(adapter, expected):
    name = adapter
    adapter = adapter.split("-", 1)[0]
    assert measured(adapter, name) == {**{"context_percent": None}, **expected}


def claude_events(*answered, usage=None, init="claude-haiku-4-5-20251001"):
    events = [{"type": "system", "subtype": "init", "model": init, "session_id": "s1"}] if init else []
    for model, parent in answered:
        events.append({"type": "assistant", "parent_tool_use_id": parent, "message": {
            "model": model, "content": [{"type": "text", "text": "OK"}],
            "usage": {"input_tokens": 10, "output_tokens": 1}}})
    events.append({"type": "result", "is_error": False, "result": "OK", "session_id": "s1",
                   "usage": {"input_tokens": 10, "output_tokens": 1}, "modelUsage": usage or {}})
    return events


def test_claude_window_comes_from_the_answering_model_never_init_or_the_largest():
    meter = context().Meter("claude")
    for event in claude_events(("claude-sonnet-5", "tool-1"), ("claude-haiku-4-5-20251001", None),
                               usage={"claude-sonnet-5": {"contextWindow": 1000000},
                                      "claude-haiku-4-5-20251001": {"contextWindow": 200000}}):
        meter.observe(event)
    assert meter.result()["context_window_tokens"] == 200000
    unmatched = context().Meter("claude")
    for event in claude_events(usage={"a": {"contextWindow": 1000000}, "b": {"contextWindow": 200000}}):
        unmatched.observe(event)
    assert unmatched.result()["context_window_tokens"] is None
    single = context().Meter("claude")
    for event in claude_events(usage={"claude-sonnet-5": {"contextWindow": 1000000}}):
        single.observe(event)
    assert single.result()["context_window_tokens"] == 1000000


def replayed(tmp_path, events):
    stream = tmp_path / "stream.jsonl"
    stream.write_text("".join(json.dumps(event) + "\n" for event in events))
    plan = supervisor().build_plan("claude", {"resolved_model": "haiku", "model_family": "anthropic",
                                              "endpoint_provider": "anthropic"}, "hello", cwd=tmp_path)
    plan["argv"] = [sys.executable, "-c", f"import sys; sys.stdout.write(open({str(stream)!r}).read())"]
    plan["grace_seconds"] = 0.1
    return supervisor().execute(plan, tmp_path / "result.md")


def fixture_events(name):
    return [json.loads(line) for line in (FIX / f"{name}.jsonl").read_text().splitlines()]


def test_plan_mode_substitution_is_attributed_to_the_answering_model(tmp_path, provider_homes):
    record = replayed(tmp_path, fixture_events("claude"))
    provenance = record["provenance"]
    assert provenance["observed_model"] == "claude-sonnet-5"
    assert provenance["observed_source"] == "claude:assistant.message.model"
    assert provenance["init_model"] == "claude-haiku-4-5-20251001"
    assert provenance["answered_models"] == ["claude-sonnet-5"]
    assert provenance["line"].startswith("Route: claude/claude-sonnet-5 ")
    assert "claude answered as claude-sonnet-5; init reported claude-haiku-4-5-20251001" in record["warnings"]
    assert record["context"]["context_window_tokens"] == 1000000


def test_direct_haiku_answers_as_haiku_without_warning(tmp_path, provider_homes):
    record = replayed(tmp_path, fixture_events("claude-direct-haiku"))
    provenance = record["provenance"]
    assert provenance["observed_model"] == provenance["init_model"] == "claude-haiku-4-5-20251001"
    assert provenance["answered_models"] == ["claude-haiku-4-5-20251001"]
    assert not any("answered as" in warning for warning in record["warnings"])
    assert record["context"]["context_window_tokens"] == 200000


def test_several_answering_models_are_all_recorded_and_the_final_one_routes(tmp_path, provider_homes):
    record = replayed(tmp_path, claude_events(("claude-sonnet-5", None), ("claude-opus-5-5", "tool-1"),
                                              ("claude-haiku-4-5-20251001", None)))
    provenance = record["provenance"]
    assert provenance["answered_models"] == ["claude-sonnet-5", "claude-opus-5-5", "claude-haiku-4-5-20251001"]
    assert provenance["observed_model"] == "claude-haiku-4-5-20251001"
    assert ("claude answered as claude-sonnet-5, claude-opus-5-5, claude-haiku-4-5-20251001; "
            "route records claude-haiku-4-5-20251001") in record["warnings"]


def test_model_usage_then_init_are_fallbacks_for_the_answering_model(tmp_path, provider_homes):
    usage = replayed(tmp_path, claude_events(usage={"claude-sonnet-5": {"contextWindow": 1000000}}))
    assert (usage["provenance"]["observed_model"], usage["provenance"]["observed_source"]) == (
        "claude-sonnet-5", "claude:result.modelUsage")
    assert "claude answered as claude-sonnet-5; init reported claude-haiku-4-5-20251001" in usage["warnings"]
    ambiguous = replayed(tmp_path, claude_events(usage={"a": {}, "b": {}}))
    assert (ambiguous["provenance"]["observed_model"], ambiguous["provenance"]["observed_source"]) == (
        "claude-haiku-4-5-20251001", "claude:init.model")
    assert not any("answered as" in warning for warning in ambiguous["warnings"])


def test_adapter_without_usage_records_null():
    meter = context().Meter("copilot")
    meter.observe({"type": "result", "result": "OK"})
    assert meter.result() == {"context_tokens": None, "input_tokens": None, "output_tokens": None,
                              "cached_input_tokens": None, "context_window_tokens": None,
                              "context_percent": None, "source": None}


def test_codex_rollout_supplies_observed_context_and_window(tmp_path):
    day = tmp_path / "sessions/2026/09/23"
    day.mkdir(parents=True)
    (day / "rollout-2026-09-23T00-00-00-thread-1.jsonl").write_text((FIX / "codex-rollout.jsonl").read_text())
    result = context().with_codex_rollout(measured("codex"), "thread-1", {"CODEX_HOME": str(tmp_path)})
    assert result["context_tokens"] == 16986
    assert result["context_window_tokens"] == 258400
    assert result["source"] == "observed"


def test_ceiling_defaults_from_product_config_and_clamps_with_warning():
    module = context()
    assert module.default_ceiling() == json.loads((ROOT / "config/model-routing.json").read_text())["context"]["ceiling_tokens"] == 300000
    assert module.clamp_ceiling(None) == (300000, None)
    assert module.clamp_ceiling(250000) == (250000, None)
    low, warning = module.clamp_ceiling(5000)
    assert low == 100000 and "clamped" in warning
    high, warning = module.clamp_ceiling(4_000_000)
    assert high == 1_000_000 and "clamped" in warning


@pytest.fixture
def provider_homes(tmp_path, monkeypatch):
    """Isolated provider state: a Codex model cache as observed on 2026-09-23, no Claude settings."""
    codex, claude = tmp_path / "codex-home", tmp_path / "claude-config"
    codex.mkdir()
    claude.mkdir()
    (codex / "models_cache.json").write_text(json.dumps({"models": [
        {"slug": "gpt-6-luna", "context_window": 272000, "effective_context_window_percent": 95,
         "max_context_window": 872000},
        {"slug": "gpt-small", "context_window": 128000, "effective_context_window_percent": 90},
    ]}))
    monkeypatch.setenv("CODEX_HOME", str(codex))
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(claude))
    return codex, claude


def plan_for(tmp_path, adapter, model, ceiling=None):
    return supervisor().build_plan(adapter, {"model": model}, "hello", cwd=tmp_path, context_ceiling=ceiling)


def has(argv, pair):
    return any(argv[i:i + 2] == pair for i in range(len(argv)))


UNKNOWN = "context_ceiling not applied: {} compaction point for {} unknown"


@pytest.mark.parametrize("adapter,model,ceiling,flag,state,tokens", [
    # Codex compacts near 95% of 272k already; the 300k default must not raise that.
    ("codex", "gpt-6-luna", None, None, "provider_default", 258400),
    ("codex", "gpt-6-luna", 200000, ["-c", "model_auto_compact_token_limit=200000"], "enforced", 200000),
    ("codex", "gpt-small", 150000, None, "provider_default", 115200),
    # A model missing from the cache has no known point: lower-only cannot be proven.
    ("codex", "gpt-unlisted", None, None, "provider_default", None),
    ("codex", "gpt-unlisted", 250000, None, "provider_default", None),
    ("claude", "opus", None, ["--autocompact", "300000"], "enforced", 300000),
    ("claude", "claude-opus-5-5", 250000, ["--autocompact", "250000"], "enforced", 250000),
    ("claude", "claude-haiku-4-5", None, None, "provider_default", 200000),
    # Read-only haiku may be answered by a 1M model in plan mode; the smaller window still bounds the flag.
    ("claude", "haiku", None, None, "provider_default", 200000),
    ("claude", "haiku", 250000, None, "provider_default", 200000),
    ("claude", "haiku", 150000, ["--autocompact", "150000"], "enforced", 150000),
    ("claude", "unknown-model", None, None, "provider_default", None),
    ("claude", "unknown-model", 250000, None, "provider_default", None),
    ("cursor", "auto", 250000, None, "unsupported", 250000),
    ("opencode", "m", 250000, None, "unsupported", 250000),
    ("kiro", "auto", 250000, None, "unsupported", 250000),
    ("agy", "m", 250000, None, "unsupported", 250000),
])
def test_ceiling_only_lowers_the_provider_compaction_point(tmp_path, provider_homes, adapter, model, ceiling,
                                                            flag, state, tokens):
    plan = plan_for(tmp_path, adapter, model, ceiling)
    applied = plan["applied"]
    assert (applied["context_ceiling"], applied["context_ceiling_tokens"]) == (state, tokens)
    assert applied["context_ceiling_requested"] == (ceiling or 300000)
    assert plan["mode"] == "read_only"
    argv = plan["argv"]
    assert not any("model_context_window" in arg for arg in argv)
    if flag:
        assert has(argv, flag)
    else:
        assert "--autocompact" not in argv
        assert not any(arg.startswith("model_auto_compact_token_limit") for arg in argv)
    unknown = [warning for warning in plan["warnings"] if warning == UNKNOWN.format(adapter, model)]
    assert len(unknown) == (1 if state == "provider_default" and tokens is None else 0)


def test_codex_without_a_models_cache_records_an_unknown_point_once(tmp_path, provider_homes):
    codex, _claude = provider_homes
    (codex / "models_cache.json").unlink()
    plan = plan_for(tmp_path, "codex", "gpt-6-luna")
    context().apply_ceiling(plan, None, supervisor().profile("codex").argv)
    assert (plan["applied"]["context_ceiling"], plan["applied"]["context_ceiling_tokens"]) == ("provider_default", None)
    assert "context_ceiling_source" not in plan["applied"]
    assert plan["warnings"].count(UNKNOWN.format("codex", "gpt-6-luna")) == 1
    assert not any(arg.startswith("model_auto_compact_token_limit") for arg in plan["argv"])


def test_claude_user_autocompact_setting_is_recorded_not_duplicated(tmp_path, provider_homes):
    _codex, claude = provider_homes
    (claude / "settings.json").write_text(json.dumps({"autoCompactEnabled": True, "autoCompactWindow": 300000}))
    plan = plan_for(tmp_path, "claude", "opus")
    assert "--autocompact" not in plan["argv"]
    assert plan["applied"]["context_ceiling"] == "provider_default"
    assert plan["applied"]["context_ceiling_tokens"] == 300000
    assert plan["applied"]["context_ceiling_source"] == "claude user settings autoCompactWindow"
    lower = plan_for(tmp_path, "claude", "opus", 250000)
    assert has(lower["argv"], ["--autocompact", "250000"])
    assert lower["applied"]["context_ceiling"] == "enforced"


@pytest.mark.parametrize("applied,threshold", [
    ({"context_ceiling": "provider_default", "context_ceiling_tokens": 258400,
      "context_ceiling_requested": 300000}, 258400),
    ({"context_ceiling": "enforced", "context_ceiling_tokens": 200000, "context_ceiling_requested": 200000}, 200000),
    ({"context_ceiling": "provider_default", "context_ceiling_tokens": None,
      "context_ceiling_requested": 300000}, 300000),
])
def test_resume_warning_uses_the_effective_ceiling(applied, threshold):
    assert context().effective_ceiling(applied) == threshold


def test_resume_warns_when_prior_context_exceeds_ceiling_or_is_unknown_without_control():
    advice = context().resume_warning
    big = {"context": {"context_tokens": 620000, "source": "observed"}}
    assert advice(big, "claude", 300000, "mcp-run1") == (
        'resuming a ~620k-token session; fresh: fabric_dispatch{prompt, handoff:"mcp-run1"}')
    assert advice(big, "cursor", 300000, "mcp-run1", task_id="two") == (
        'resuming a ~620k-token session; fresh: fabric_dispatch{prompt, handoff:"mcp-run1", task_id:"two"}')
    assert advice({"context": {"context_tokens": 212000}}, "cursor", 300000, "r") is None
    unknown = {"context": None}
    assert advice(unknown, "claude", 300000, "r") is None
    assert advice(unknown, "codex", 300000, "r") is None
    assert advice(unknown, "cursor", 300000, "r").startswith("resuming a session of unknown size;")
    assert advice({}, "kiro", 300000, "r").startswith("resuming a session of unknown size;")


@pytest.mark.parametrize("value,marker", [
    ({"context_tokens": 212000, "context_window_tokens": 1000000, "source": "observed"}, " · ctx 212k/1M"),
    ({"context_tokens": 16986, "context_window_tokens": 258400, "source": "observed"}, " · ctx 17k/258k"),
    ({"context_tokens": 19124, "context_window_tokens": None, "source": "estimated"}, " · ctx ~19k"),
    ({"context_tokens": None, "context_percent": 7.73, "source": "observed"}, " · ctx 8%"),
    ({"context_tokens": None, "source": None}, ""),
    (None, ""),
])
def test_context_marker_is_compact(value, marker):
    assert context().marker(value) == marker


def test_terminal_digest_carries_context_on_the_route_line():
    records = importlib.import_module("skills.orchestrate.scripts.fabric_records")
    row = json.loads((ROOT / "tests/fixtures/fabric-v1/attempt.json").read_text())
    row["context"] = {"context_tokens": 212000, "context_window_tokens": 1000000, "source": "observed"}
    lines = records.render_digest(row).splitlines()
    assert lines[1] == "  " + row["provenance"]["line"] + " · ctx 212k/1M"
    assert "ctx" not in row["provenance"]["line"]
