"""Direct Codex wrappers keep report paths stable across caller and worker cwd."""

from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def test_codex_wrappers_anchor_scratch_at_primary_checkout():
    for name in ("codex-analyst.md", "codex-implementer.md"):
        text = (ROOT / "agents" / name).read_text()
        assert 'ROOT=$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")' in text
        assert 'SCRATCH="$ROOT/.agent-run/scratch"' in text
        assert '-o "$SCRATCH/codex-<slug>-report.md"' in text
        assert "< .agent-run/scratch/" not in text
        assert "> .agent-run/scratch/" not in text
