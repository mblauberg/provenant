import importlib.util
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "lint-timebomb-fixtures.py"


def load_module():
    spec = importlib.util.spec_from_file_location("lint_timebomb_fixtures", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


def test_finds_fixed_iso_date_in_now_relative_window(tmp_path):
    module = load_module()
    path = tmp_path / "test_fixture.py"
    path.write_text(
        'from datetime import datetime, timedelta\n'
        'def test_window():\n'
        '    fixture = "2026-10-23T12:00:00Z"\n'
        '    assert datetime.now() - timedelta(days=1) < parse(fixture)\n',
        encoding="utf-8",
    )
    assert module.scan_file(path) == [(3, "2026-10-23T12:00:00Z")]


def test_finds_fixed_date_prefix_in_joined_string_window(tmp_path):
    module = load_module()
    path = tmp_path / "test_fixture.py"
    path.write_text(
        'from datetime import datetime, timedelta\n'
        'def test_fixture_state():\n'
        '    for i in range(1, 16):\n'
        '        stamp = f"2026-10-22T00:10:{i:02d}.000Z"\n'
        '        assert datetime.now() - timedelta(days=1) < parse(stamp)\n',
        encoding="utf-8",
    )
    assert module.scan_file(path) == [(4, "2026-10-22")]


def test_allows_intentional_fixed_date_and_ignores_static_date(tmp_path):
    module = load_module()
    path = tmp_path / "test_fixture.py"
    path.write_text(
        'def test_window():\n'
        '    fixture = "2026-10-23"  # timebomb: allow boundary regression case\n'
        '    assert datetime.now() > parse(fixture)\n'
        'def test_static():\n'
        '    fixture = "2026-10-23"\n',
        encoding="utf-8",
    )
    assert module.scan_file(path) == []


def test_scans_typescript_relative_date_fixtures(tmp_path):
    module = load_module()
    path = tmp_path / "lanes.test.ts"
    path.write_text(
        'it("uses a fixed stamp", () => {\n'
        '  const stamp = "2026-10-23T12:00:00.000Z";\n'
        '  const day = "2026-10-23";\n'
        '  const cutoff = Date.now() - 60_000;\n'
        '});\n',
        encoding="utf-8",
    )
    assert module.scan_file(path) == [
        (2, "2026-10-23T12:00:00.000Z"),
        (3, "2026-10-23"),
    ]


def test_changed_comparison_covers_unchanged_fixture_literal(tmp_path):
    module = load_module()
    path = tmp_path / "test_fixture.py"
    path.write_text(
        'def test_window():\n'
        '    fixture = "2026-10-23T12:00:00Z"\n'
        '    assert datetime.now() > parse(fixture)\n',
        encoding="utf-8",
    )
    assert module._context_changed(path, 2, {3})


def test_finds_module_fixture_when_test_context_uses_now(tmp_path):
    module = load_module()
    path = tmp_path / "test_fixture.py"
    path.write_text(
        'FIXTURE = "2026-10-23T12:00:00Z"\n'
        'def test_window():\n'
        '    assert datetime.now() > parse(FIXTURE)\n',
        encoding="utf-8",
    )
    assert module.scan_file(path) == [(1, "2026-10-23T12:00:00Z")]
    assert module._context_changed(path, 1, {3})


def test_ignores_module_fixture_unused_by_now_relative_test(tmp_path):
    module = load_module()
    path = tmp_path / "test_fixture.py"
    path.write_text(
        'FIXTURE = "2026-10-23T12:00:00Z"\n'
        'def test_window():\n'
        '    assert datetime.now() > datetime(2026, 10, 23)\n'
        'def test_unrelated():\n'
        '    assert True\n',
        encoding="utf-8",
    )

    assert module.scan_file(path) == []


def test_module_fixture_does_not_follow_unrelated_changed_test(tmp_path):
    module = load_module()
    path = tmp_path / "test_fixture.py"
    path.write_text(
        'FIXTURE = "2026-10-23T12:00:00Z"\n'
        'def test_window():\n'
        '    assert datetime.now() > parse(FIXTURE)\n'
        'def test_unrelated():\n'
        '    assert True\n',
        encoding="utf-8",
    )

    assert not module._context_changed(path, 1, {5})


def test_module_fixture_name_shadowed_by_local_is_not_a_reference(tmp_path):
    module = load_module()
    path = tmp_path / "test_fixture.py"
    path.write_text(
        'STAMP = "2026-10-23T12:00:00Z"\n'
        'def test_window():\n'
        '    STAMP = "static data"\n'
        '    assert datetime.now() > parse(STAMP)\n',
        encoding="utf-8",
    )

    assert module.scan_file(path) == []


def test_nested_local_shadow_does_not_hide_outer_module_fixture(tmp_path):
    module = load_module()
    path = tmp_path / "test_fixture.py"
    path.write_text(
        'STAMP = "2026-10-23T12:00:00Z"\n'
        'def test_window():\n'
        '    def inner():\n'
        '        STAMP = "static data"\n'
        '    assert datetime.now() > parse(STAMP)\n',
        encoding="utf-8",
    )

    assert module.scan_file(path) == [(1, "2026-10-23T12:00:00Z")]


def test_changed_local_shadow_does_not_match_module_fixture(tmp_path):
    module = load_module()
    path = tmp_path / "test_fixture.py"
    path.write_text(
        'STAMP = "2026-10-23T12:00:00Z"\n'
        'def test_window():\n'
        '    assert datetime.now() > parse(STAMP)\n'
        'def test_changed():\n'
        '    STAMP = "static data"\n'
        '    assert datetime.now() > parse(STAMP)\n',
        encoding="utf-8",
    )

    assert not module._context_changed(path, 1, {5})


def test_changed_typescript_test_covers_module_fixture(tmp_path):
    module = load_module()
    path = tmp_path / "lanes.test.ts"
    path.write_text(
        'const fixture = "2026-10-23";\n'
        'it("uses fixture", () => {\n'
        '  const cutoff = Date.now() - 60_000;\n'
        '});\n',
        encoding="utf-8",
    )
    assert module._context_changed(path, 1, {3})


def test_finds_date_in_referenced_helper_used_by_changed_window_test(tmp_path):
    module = load_module()
    path = tmp_path / "test_fixture.py"
    path.write_text(
        'def fixed_stamp():\n'
        '    return "2026-10-23T12:00:00Z"\n'
        'def test_window():\n'
        '    assert datetime.now() > parse(fixed_stamp())\n',
        encoding="utf-8",
    )
    assert module.scan_file(path) == [(2, "2026-10-23T12:00:00Z")]
    assert module._context_changed(path, 2, {4})


def test_does_not_treat_a_shadowed_helper_name_as_a_fixture_reference(tmp_path):
    module = load_module()
    path = tmp_path / "test_fixture.py"
    path.write_text(
        'def fixture():\n'
        '    return "2026-10-23T12:00:00Z"\n'
        'def test_window():\n'
        '    fixture = "static data"\n'
        '    assert datetime.now() > parse(fixture)\n',
        encoding="utf-8",
    )
    assert module.scan_file(path) == []


def test_missing_merge_base_warns_and_skips_lint(tmp_path, capsys):
    module = load_module()

    assert module.main([str(tmp_path)]) == 0
    assert "warning: could not find merge-base with origin/main" in capsys.readouterr().err
