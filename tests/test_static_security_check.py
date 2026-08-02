import importlib.util
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "static-security-check.py"


def load_module():
    spec = importlib.util.spec_from_file_location("static_security_check", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader
    spec.loader.exec_module(module)
    return module


def test_static_security_check_accepts_safe_calls_and_rejects_high_risk_patterns(tmp_path):
    module = load_module()
    (tmp_path / "safe.py").write_text("import subprocess\nsubprocess.run(['git', 'status'], check=False)\n")
    assert module.scan(tmp_path) == []
    (tmp_path / "unsafe.py").write_text("import subprocess\nsubprocess.run('echo unsafe', shell=True)\neval('1+1')\n")
    assert {item["rule"] for item in module.scan(tmp_path)} == {"subprocess-shell-true", "dangerous-dynamic-call"}


def test_static_security_check_resolves_import_aliases_and_safe_yaml_loader(tmp_path):
    module = load_module()
    (tmp_path / "aliases.py").write_text(
        "import subprocess as sp\nfrom os import system as run_system\n"
        "from pickle import loads as unpickle\n"
        "sp.run('x', shell=True)\nrun_system('x')\nunpickle(b'x')\n"
    )
    rules = [item["rule"] for item in module.scan_file(tmp_path / "aliases.py")]
    assert rules.count("dangerous-dynamic-call") == 2
    assert "subprocess-shell-true" in rules
    (tmp_path / "safe_yaml.py").write_text("import yaml as y\ny.load('x', Loader=y.SafeLoader)\n")
    assert module.scan_file(tmp_path / "safe_yaml.py") == []


def test_static_security_check_follows_legal_import_and_assignment_aliases_in_order(tmp_path):
    module = load_module()
    path = tmp_path / "ordered_aliases.py"
    path.write_text(
        "os.system('before-import')\n"
        "import os.path\n"
        "os.system('after-import')\n"
        "import subprocess\n"
        "runner = subprocess\n"
        "runner.run('x', shell=True)\n"
        "runner = object()\n"
        "runner.run('safe-shadow', shell=True)\n"
    )
    findings = module.scan_file(path)
    assert [(item["rule"], item["line"]) for item in findings] == [
        ("dangerous-dynamic-call", 3),
        ("subprocess-shell-true", 6),
    ]


def test_static_security_check_scans_definitions_nested_under_match_cases(tmp_path):
    module = load_module()
    path = tmp_path / "match_case.py"
    path.write_text(
        "import os\n"
        "import subprocess\n"
        "match {'kind': 'unsafe'}:\n"
        "    case {'kind': 'unsafe'}:\n"
        "        def nested():\n"
        "            os.system('unsafe')\n"
        "        class Runner:\n"
        "            subprocess.run('unsafe', shell=True)\n"
    )

    assert [(item["rule"], item["line"]) for item in module.scan_file(path)] == [
        ("dangerous-dynamic-call", 6),
        ("subprocess-shell-true", 8),
    ]


def test_match_pattern_bindings_shadow_import_aliases(tmp_path):
    module = load_module()
    path = tmp_path / "match_shadow.py"
    path.write_text(
        "import os\n"
        "match {'tool': object()}:\n"
        "    case {'tool': os}:\n"
        "        os.system('ordinary method')\n"
    )

    assert module.scan_file(path) == []


def test_match_pattern_bindings_do_not_shadow_imports_after_match(tmp_path):
    module = load_module()
    path = tmp_path / "match_postlude.py"
    path.write_text(
        "import os\n"
        "match {'other': object()}:\n"
        "    case {'tool': os}:\n"
        "        pass\n"
        "os.system('unsafe')\n"
    )

    assert [(item["rule"], item["line"]) for item in module.scan_file(path)] == [
        ("dangerous-dynamic-call", 5),
    ]


def test_repository_python_surface_passes_static_security_check():
    assert load_module().scan(ROOT) == []


def test_pipe_wait_rule_finds_one_bad_fixture_and_accepts_bounded_drains(tmp_path):
    module = load_module()
    (tmp_path / "bad.py").write_text(
        "import subprocess as sp\n"
        "def bad():\n"
        "    process = sp.Popen(['tool'], stdout=sp.PIPE, stderr=sp.PIPE)\n"
        "    process.wait(timeout=1)\n",
        encoding="utf-8",
    )
    (tmp_path / "regular_file.py").write_text(
        "import subprocess\n"
        "def safe(output):\n"
        "    process = subprocess.Popen(['tool'], stdout=output)\n"
        "    process.wait(timeout=1)\n",
        encoding="utf-8",
    )
    (tmp_path / "communicate.py").write_text(
        "import subprocess\n"
        "def safe():\n"
        "    process = subprocess.Popen(['tool'], stdout=subprocess.PIPE, stderr=subprocess.PIPE)\n"
        "    process.communicate(timeout=1)\n"
        "    process.wait(timeout=1)\n",
        encoding="utf-8",
    )
    (tmp_path / "selector.py").write_text(
        "import selectors, subprocess\n"
        "def safe():\n"
        "    process = subprocess.Popen(['tool'], stdout=subprocess.PIPE, stderr=subprocess.PIPE)\n"
        "    selector = selectors.DefaultSelector()\n"
        "    selector.register(process.stdout, selectors.EVENT_READ)\n"
        "    selector.register(process.stderr, selectors.EVENT_READ)\n"
        "    while selector.get_map():\n"
        "        for key, _ in selector.select(timeout=1):\n"
        "            key.fileobj.read()\n"
        "    process.wait(timeout=1)\n",
        encoding="utf-8",
    )
    (tmp_path / "read_and_iteration.py").write_text(
        "import subprocess\n"
        "def read_safe():\n"
        "    process = subprocess.Popen(['tool'], stdout=subprocess.PIPE)\n"
        "    process.stdout.read()\n"
        "    process.wait(timeout=1)\n"
        "def iteration_safe():\n"
        "    process = subprocess.Popen(['tool'], stdout=subprocess.PIPE)\n"
        "    for line in process.stdout:\n"
        "        consume(line)\n"
        "    process.wait(timeout=1)\n",
        encoding="utf-8",
    )
    (tmp_path / "concurrent_reader.py").write_text(
        "import subprocess, threading\n"
        "def safe(drain):\n"
        "    process = subprocess.Popen(['tool'], stdout=subprocess.PIPE)\n"
        "    reader = threading.Thread(target=drain, args=(process.stdout,))\n"
        "    reader.start()\n"
        "    process.wait(timeout=1)\n",
        encoding="utf-8",
    )

    findings = [
        finding
        for finding in module.scan(tmp_path)
        if finding["rule"] == "subprocess-pipe-wait-before-drain"
    ]

    assert len(findings) == 1
    assert findings[0]["path"].endswith("bad.py")
    assert findings[0]["line"] == 4


def test_pipe_wait_rule_conservatively_covers_bindings_and_branches(tmp_path):
    module = load_module()
    fixtures = {
        "registration_only.py": (
            "import selectors, subprocess\n"
            "def bad():\n"
            "    process = subprocess.Popen(['tool'], stdout=subprocess.PIPE, stderr=subprocess.PIPE)\n"
            "    selector = selectors.DefaultSelector()\n"
            "    selector.register(process.stdout, selectors.EVENT_READ)\n"
            "    selector.register(process.stderr, selectors.EVENT_READ)\n"
            "    process.wait()\n"
        ),
        "conditional_drain.py": (
            "import subprocess\n"
            "def bad(flag):\n"
            "    process = subprocess.Popen(['tool'], stdout=subprocess.PIPE)\n"
            "    if flag:\n"
            "        process.stdout.read()\n"
            "    process.wait()\n"
        ),
        "conditional_rebind.py": (
            "import subprocess\n"
            "def bad(flag, output):\n"
            "    if flag:\n"
            "        process = subprocess.Popen(['tool'], stdout=subprocess.PIPE)\n"
            "    else:\n"
            "        process = subprocess.Popen(['tool'], stdout=output)\n"
            "    process.wait()\n"
        ),
        "context_manager.py": (
            "import subprocess\n"
            "def bad():\n"
            "    with subprocess.Popen(['tool'], stdout=subprocess.PIPE) as process:\n"
            "        process.wait()\n"
        ),
        "named_expression.py": (
            "import subprocess\n"
            "def bad():\n"
            "    (process := subprocess.Popen(['tool'], stdout=subprocess.PIPE))\n"
            "    process.wait()\n"
        ),
        "unrecognised_start.py": (
            "import subprocess\n"
            "def bad(worker):\n"
            "    process = subprocess.Popen(['tool'], stdout=subprocess.PIPE)\n"
            "    worker(process.stdout).start()\n"
            "    process.wait()\n"
        ),
    }
    for name, source in fixtures.items():
        (tmp_path / name).write_text(source, encoding="utf-8")

    findings = [
        finding
        for finding in module.scan(tmp_path)
        if finding["rule"] == "subprocess-pipe-wait-before-drain"
    ]

    assert len(findings) == len(fixtures)
    assert {Path(finding["path"]).name for finding in findings} == set(fixtures)


def test_pipe_wait_rule_is_clean_across_all_four_real_execution_sites():
    module = load_module()
    sites = (
        ROOT / "scripts" / "public_release_check.py",
        ROOT / "skills" / "deliver" / "scripts" / "delivery_receipt_process.py",
        ROOT / "scripts" / "change_gate_runner.py",
        ROOT / "skills" / "orchestrate" / "evals" / "test_cf_dispatch.py",
    )

    findings = [
        finding
        for site in sites
        for finding in module.scan_file(site)
        if finding["rule"] == "subprocess-pipe-wait-before-drain"
    ]

    assert findings == []
