import importlib.util
from pathlib import Path
from subprocess import CompletedProcess


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "retry-npm-install.py"


def load_module():
    spec = importlib.util.spec_from_file_location("retry_npm_install", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


def test_retries_only_transient_npm_registry_failures(monkeypatch):
    module = load_module()
    results = iter([
        CompletedProcess([], 1, "", "npm ERR! code ECONNRESET"),
        CompletedProcess([], 0, "installed", ""),
    ])
    calls = []
    monkeypatch.setattr(module.subprocess, "run", lambda command, **kwargs: (calls.append(command), next(results))[1])
    delays = []
    monkeypatch.setattr(module.time, "sleep", delays.append)

    assert module.main(["--", "npm", "ci"]) == 0
    assert calls == [["npm", "ci"], ["npm", "ci"]]
    assert delays == [2]


def test_does_not_retry_other_install_failures(monkeypatch):
    module = load_module()
    calls = []
    monkeypatch.setattr(
        module.subprocess, "run",
        lambda command, **kwargs: (calls.append(command), CompletedProcess([], 1, "", "npm ERR! EACCES"))[1],
    )
    monkeypatch.setattr(module.time, "sleep", lambda delay: (_ for _ in ()).throw(AssertionError("unexpected retry")))

    assert module.main(["npm", "ci"]) == 1
    assert calls == [["npm", "ci"]]


def test_does_not_retry_unrelated_503_output(monkeypatch):
    module = load_module()
    calls = []
    monkeypatch.setattr(
        module.subprocess, "run",
        lambda command, **kwargs: (calls.append(command), CompletedProcess([], 1, "package says 503", ""))[1],
    )
    monkeypatch.setattr(module.time, "sleep", lambda delay: (_ for _ in ()).throw(AssertionError("unexpected retry")))

    assert module.main(["npm", "ci"]) == 1
    assert calls == [["npm", "ci"]]
