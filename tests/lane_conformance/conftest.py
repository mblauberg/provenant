"""FIFO-controlled real dispatch fixtures."""

import os
from pathlib import Path

import pytest

from tests.support.dispatch import ROOT

from .support import Lane


def pytest_configure(config):
    impl = os.environ.get("PROVENANT_LANE_IMPL", "python")
    if impl not in {"python", "rust"}:
        raise pytest.UsageError(f"unsupported PROVENANT_LANE_IMPL={impl!r}")
    config.addinivalue_line("markers", "rust_only: requires the S2 Rust supervisor")


def pytest_collection_modifyitems(items):
    if os.environ.get("PROVENANT_LANE_IMPL", "python") == "python":
        for item in items:
            if item.get_closest_marker("rust_only"):
                item.add_marker(pytest.mark.xfail(
                    strict=False, reason="rust-only until S2", run=False))


def rust_binary():
    directory = Path(os.environ.get("PROVENANT_BIN_DIR", ROOT / "tools/front-door/target/debug"))
    return Path(os.environ.get("PROVENANT_FRONT_DOOR_BIN", directory / "provenant")).resolve()


@pytest.fixture(scope="session", params=[os.environ.get("PROVENANT_LANE_IMPL", "python")])
def impl(request):
    if request.param == "rust":
        binary = rust_binary()
        if not binary.is_file() or not os.access(binary, os.X_OK):
            pytest.skip("Rust lane binary is absent; set PROVENANT_FRONT_DOOR_BIN")
    return request.param


@pytest.fixture
def lane(tmp_path, monkeypatch, impl):
    lanes = []
    if impl == "rust":
        bindir = tmp_path / "rust-bin"
        bindir.mkdir()
        (bindir / "provenant").symlink_to(rust_binary())

    def create(name="lane", startup=30, **options):
        driver = Lane(tmp_path / name, monkeypatch, startup, impl=impl, **options)
        if impl == "rust":
            driver.env["PROVENANT_BIN_DIR"] = str(bindir)
        lanes.append(driver)
        return driver

    yield create
    first_error = None
    for driver in reversed(lanes):
        try:
            driver.close()
        except BaseException as error:
            if first_error is None:
                first_error = error
    if first_error is not None:
        raise first_error
