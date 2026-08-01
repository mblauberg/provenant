from pathlib import Path

import pytest


try:
    from scripts.count_skill_words import count_skill_words, word_count_diagnostics
except (ImportError, AttributeError):
    count_skill_words = None
    word_count_diagnostics = None


def test_frontmatter_only_is_counted():
    assert callable(count_skill_words), "missing function: count_skill_words"
    assert count_skill_words("---\nname: demo\ndescription: Use demo\n---\n") == 5


def test_body_only_is_counted():
    assert callable(count_skill_words), "missing function: count_skill_words"
    assert count_skill_words("Use the tool carefully.") == 4


def test_frontmatter_and_body_are_both_counted():
    assert callable(count_skill_words), "missing function: count_skill_words"
    text = "---\nname: demo\ndescription: Use demo\n---\nUse the tool carefully."
    assert count_skill_words(text) == 9


def test_markdown_link_counts_text_but_not_url():
    assert callable(count_skill_words), "missing function: count_skill_words"
    assert count_skill_words("Read [run-contract.md](references/run-contract.md) now.") == 4


@pytest.mark.parametrize(
    ("word_count", "expected_diagnostics"),
    (
        (459, ()),
        (460, ("warning",)),
        (500, ()),
        (501, ("error",)),
    ),
)
def test_word_count_thresholds(tmp_path: Path, word_count: int, expected_diagnostics: tuple[str, ...]):
    assert callable(word_count_diagnostics), "missing function: word_count_diagnostics"
    path = tmp_path / "SKILL.md"
    path.write_text("word " * word_count)

    diagnostics = word_count_diagnostics(path)

    assert tuple(item.split(":", 1)[0] for item in diagnostics) == expected_diagnostics
    assert all(str(word_count) in item for item in diagnostics)

