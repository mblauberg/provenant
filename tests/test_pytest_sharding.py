import importlib.util
from pathlib import Path
from types import SimpleNamespace
import zlib


ROOT = Path(__file__).resolve().parents[1]
ROOT_CONFTEST_SPEC = importlib.util.spec_from_file_location(
    "provenant_root_tests_conftest", ROOT / "tests" / "conftest.py"
)
assert ROOT_CONFTEST_SPEC is not None and ROOT_CONFTEST_SPEC.loader is not None
conftest = importlib.util.module_from_spec(ROOT_CONFTEST_SPEC)
ROOT_CONFTEST_SPEC.loader.exec_module(conftest)


def test_collection_hook_pins_fabric_hosts_and_partitions_other_files(monkeypatch) -> None:
    monkeypatch.setenv(conftest.LIVE_PROVIDER_ENV, "1")
    paths = [
        conftest.ROOT / "tests/test_fabric_hosts.py",
        conftest.ROOT / "tests/test_alpha.py",
        conftest.ROOT / "tests/test_beta.py",
    ]
    memberships: dict[Path, list[int]] = {path: [] for path in paths}

    for index in range(1, 4):
        selected = [SimpleNamespace(path=path) for path in paths]
        deselected = []
        config = SimpleNamespace(
            hook=SimpleNamespace(pytest_deselected=lambda items: deselected.extend(items))
        )
        monkeypatch.setenv("PROVENANT_TEST_SHARD", f"{index}/3")
        conftest.pytest_collection_modifyitems(config, selected)
        for item in selected:
            memberships[Path(item.path)].append(index)
        assert {Path(item.path) for item in selected + deselected} == set(paths)
        assert len(selected) + len(deselected) == len(paths)

    assert memberships[paths[0]] == [1]
    for path in paths[1:]:
        relative = path.relative_to(conftest.ROOT).as_posix()
        expected = zlib.crc32(relative.encode("utf-8")) % 2 + 2
        assert memberships[path] == [expected]
