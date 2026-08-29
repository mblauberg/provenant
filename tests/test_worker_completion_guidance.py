from pathlib import Path
import re


ROOT = Path(__file__).resolve().parents[1]


GUIDANCE_FILES = (
    ROOT / "skills/orchestrate/references/worker-liveness.md",
    ROOT / "agents/codex-analyst.md",
    ROOT / "agents/codex-implementer.md",
)


def _active_fifo_completion_patterns() -> tuple[re.Pattern[str], ...]:
    return (
        re.compile(r"\bmkfifo\b", re.IGNORECASE),
        re.compile(r"\b(?:done|codex-[^\s`]+)\.fifo\b", re.IGNORECASE),
        re.compile(r"\bcat\s+[^\n]*\.fifo\b", re.IGNORECASE),
    )


def test_worker_completion_guidance_does_not_use_fifo_rendezvous():
    source = GUIDANCE_FILES[0].read_text()

    for pattern in _active_fifo_completion_patterns():
        assert not pattern.search(source), pattern.pattern

    assert "run the worker in the foreground" in source
    assert 'wait "$PID"' in source
    assert "regular completion file" in source
    assert "foreground wait observes\nthat PID's exit" in source


def test_codex_agent_definitions_use_foreground_or_durable_pid_completion():
    for path in GUIDANCE_FILES[1:]:
        source = path.read_text()
        for pattern in _active_fifo_completion_patterns():
            assert not pattern.search(source), f"{path}: {pattern.pattern}"
        assert "foreground" in source.lower(), path
        assert 'wait "$PID"' in source or "regular completion file" in source, path
