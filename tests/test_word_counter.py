import subprocess
from pathlib import Path

import pytest


from scripts import count_skill_words as word_counter
from scripts.count_skill_words import (
    count_skill_words,
    word_count_delta_warnings,
    word_count_diagnostics,
)


def test_frontmatter_only_is_counted():
    assert count_skill_words("---\nname: demo\ndescription: Use demo\n---\n") == 5


def test_body_only_is_counted():
    assert count_skill_words("Use the tool carefully.") == 4


def test_frontmatter_and_body_are_both_counted():
    text = "---\nname: demo\ndescription: Use demo\n---\nUse the tool carefully."
    assert count_skill_words(text) == 9


def test_markdown_link_is_not_reading_burden():
    assert count_skill_words("Read [run-contract.md](references/run-contract.md) now.") == 2


def test_markdown_link_preserves_natural_label_prose():
    text = "See [the deployment guide](references/deploy.md) for details."
    assert count_skill_words(text) == 6


def test_bare_document_path_is_not_reading_burden():
    assert count_skill_words("Read references/run-contract.md now.") == 2


def test_reference_link_and_definition_are_not_reading_burden():
    text = "Read [run-contract.md][contract] now.\n\n[contract]: references/run-contract.md"
    assert count_skill_words(text) == 2


def test_reference_link_preserves_natural_label_prose():
    text = "See [the deployment guide][guide] for details.\n\n[guide]: references/deploy.md"
    assert count_skill_words(text) == 6


def test_bracketed_prose_is_not_treated_as_a_reference_definition():
    assert count_skill_words("[note]: this is ordinary prose") == 5


def test_path_filter_keeps_ordinary_slash_and_hyphen_prose():
    assert count_skill_words("The phrase a/b and well-known prose remains.") == 8


@pytest.mark.parametrize(
    ("word_count", "expected_diagnostics"),
    (
        (459, ()),
        (460, ()),
        (500, ()),
        (501, ("error",)),
    ),
)
def test_word_count_thresholds(tmp_path: Path, word_count: int, expected_diagnostics: tuple[str, ...]):
    path = tmp_path / "SKILL.md"
    path.write_text("word " * word_count)

    diagnostics = word_count_diagnostics(path)

    assert tuple(item.split(":", 1)[0] for item in diagnostics) == expected_diagnostics
    assert all(str(word_count) in item for item in diagnostics)


def test_delta_warns_increases_at_risk(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    repository, base_sha, head_sha = _git_skill_history(tmp_path, 491, 495)
    monkeypatch.setattr(word_counter, "REPOSITORY_ROOT", repository)

    delta_warnings = word_count_delta_warnings(base_sha, head_sha)

    assert any("+4" in warning for warning in delta_warnings)


def _git_skill_history(tmp_path: Path, old_count: int, new_count: int) -> tuple[Path, str, str]:
    repository = tmp_path / f"repo-{old_count}-{new_count}"
    skill = repository / "skills" / "demo" / "SKILL.md"
    skill.parent.mkdir(parents=True)
    skill.write_text("word " * old_count)
    subprocess.run(["git", "init", "-q", str(repository)], check=True)
    subprocess.run(["git", "-C", str(repository), "add", "."], check=True)
    commit = ["git", "-C", str(repository), "-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-qm"]
    subprocess.run([*commit, "base"], check=True)
    base_sha = subprocess.check_output(["git", "-C", str(repository), "rev-parse", "HEAD"], text=True).strip()
    skill.write_text("word " * new_count)
    subprocess.run(["git", "-C", str(repository), "add", "."], check=True)
    if new_count == old_count:
        head_sha = base_sha
    else:
        subprocess.run([*commit, "head"], check=True)
        head_sha = subprocess.check_output(["git", "-C", str(repository), "rev-parse", "HEAD"], text=True).strip()
    return repository, base_sha, head_sha


@pytest.mark.parametrize(
    ("old_count", "new_count", "expected_fragment"),
    (
        (459, 468, None),
        (459, 469, "+10"),
        (465, 469, None),
        (465, 470, "+5"),
        (484, 488, None),
        (484, 489, "+5"),
        (489, 490, "+1"),
        (499, 500, None),
        (499, 501, "error"),
    ),
)
def test_word_count_deltas(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    old_count: int,
    new_count: int,
    expected_fragment: str | None,
):
    repository, base_sha, head_sha = _git_skill_history(tmp_path, old_count, new_count)
    monkeypatch.setattr(word_counter, "REPOSITORY_ROOT", repository)

    diagnostics = word_count_delta_warnings(base_sha, head_sha)

    if expected_fragment is None:
        assert diagnostics == ()
    else:
        assert any(expected_fragment in diagnostic for diagnostic in diagnostics)
