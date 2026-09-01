"""Ownership contracts for the instance side of an installation (ADR 0019).

Every test here runs against scratch roots. Nothing in this file may read or
write the real `~/.agents`, `~/.claude` or `~/.codex`.
"""

from pathlib import Path
import json
import os
import shutil
import subprocess
import sys

import pytest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "instance_installation.py"

sys.path.insert(0, str(ROOT / "scripts"))
import instance_installation as instance  # noqa: E402


def build_product(tmp_path: Path) -> Path:
    """A minimal product tree carrying exactly the templates the seeder reads."""
    product = tmp_path / "product"
    (product / "config").mkdir(parents=True)
    (product / "package.json").write_text(json.dumps({"version": "1.2.3"}) + "\n")
    (product / "AGENTS.md").write_text("# Product doctrine\n")
    (product / "config" / "model-preferences.json").write_text('{"shipped": true}\n')
    (product / "config" / "model-routing.json").write_text('{"shipped": true}\n')
    return product


def run(action: str, product: Path, instance_root: Path, *extra: str):
    return subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            action,
            "--product-root",
            str(product),
            "--instance-root",
            str(instance_root),
            *extra,
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def seed(product: Path, instance_root: Path) -> dict:
    result = run("seed", product, instance_root)
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def test_desired_state_is_seeded_with_product_version_and_split_mode(tmp_path):
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()

    result = seed(product, instance_root)

    assert result["desired_state_state"] == "created"
    assert result["mode"] == "split"
    written = json.loads((instance_root / "config" / "installation.json").read_text())
    assert written == {
        "schema_version": 1,
        "contract": "installation-desired-state",
        "product": {"name": "provenant", "version": "1.2.3"},
        "mode": "split",
    }


def test_desired_state_records_a_fused_layout_when_the_roots_are_one_tree(tmp_path):
    product = build_product(tmp_path)

    result = seed(product, product)

    assert result["desired_state"]["mode"] == "fused"


def test_desired_state_is_path_free_and_therefore_committable(tmp_path):
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()
    seed(product, instance_root)

    text = (instance_root / "config" / "installation.json").read_text()

    # The machine-specific facts are exactly what must not survive a commit.
    for machine_local in (str(tmp_path), str(product), str(instance_root), "/", "~"):
        assert machine_local not in text, f"desired state leaked {machine_local!r}"


def test_desired_state_rejects_an_absolute_path_in_any_field(tmp_path):
    document = {
        "schema_version": 1,
        "contract": "installation-desired-state",
        "product": {"name": "provenant", "version": "1.2.3", "ref": "/opt/provenant"},
        "mode": "split",
    }

    with pytest.raises(instance.InstallError, match="path-free"):
        instance.validate_desired_state(document)


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("schema_version", 2),
        ("contract", "installation-receipt"),
        ("mode", "linked"),
    ],
)
def test_desired_state_rejects_an_unsupported_header(field, value):
    document = {
        "schema_version": 1,
        "contract": "installation-desired-state",
        "product": {"name": "provenant", "version": "1.2.3"},
        "mode": "split",
    }
    document[field] = value

    with pytest.raises(instance.InstallError):
        instance.validate_desired_state(document)


def test_desired_state_is_never_rewritten_on_a_later_install(tmp_path):
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()
    seed(product, instance_root)
    path = instance_root / "config" / "installation.json"
    # The instance pins an older product than the one now installing.
    pinned = {
        "schema_version": 1,
        "contract": "installation-desired-state",
        "product": {"name": "provenant", "version": "1.0.0", "ref": "v1.0.0"},
        "mode": "split",
    }
    path.write_text(json.dumps(pinned, indent=2) + "\n")
    (product / "package.json").write_text(json.dumps({"version": "9.9.9"}) + "\n")

    result = seed(product, instance_root)

    assert result["desired_state_state"] == "existing"
    assert json.loads(path.read_text()) == pinned


def test_seeded_files_are_written_once_and_never_overwritten_on_update(tmp_path):
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()

    first = seed(product, instance_root)

    assert {item["path"]: item["state"] for item in first["seeded"]} == {
        "AGENTS.md": "created",
        "config/model-preferences.json": "created",
        "config/model-routing.json": "created",
    }
    for relative in instance.SEEDED_FILES:
        assert (instance_root / relative).read_text() == (product / relative).read_text()

    # The user edits their copies and the product ships new templates.
    for relative in instance.SEEDED_FILES:
        (instance_root / relative).write_text("mine\n")
        (product / relative).write_text("newer product template\n")

    second = seed(product, instance_root)

    assert {item["state"] for item in second["seeded"]} == {"existing"}
    for relative in instance.SEEDED_FILES:
        assert (instance_root / relative).read_text() == "mine\n"


def test_seeding_never_writes_a_receipt_into_the_instance_root(tmp_path):
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()

    seed(product, instance_root)

    written = {
        path.relative_to(instance_root).as_posix()
        for path in instance_root.rglob("*")
        if path.is_file()
    }
    assert written == {
        "config/installation.json",
        ".agent-fabric/product-root.json",
        ".agent-fabric/.gitignore",
        "AGENTS.md",
        "config/model-preferences.json",
        "config/model-routing.json",
    }


def test_the_product_path_lives_only_in_the_ignored_pointer(tmp_path):
    """Committed instance state never carries an absolute machine path."""
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()

    result = seed(product, instance_root)

    assert result["product_pointer"] == {
        "schema_version": 1,
        "product_root": str(product.resolve()),
    }
    pointer = instance_root / ".agent-fabric" / "product-root.json"
    assert json.loads(pointer.read_text())["product_root"] == str(product.resolve())

    committed = {
        path
        for path in instance_root.rglob("*")
        if path.is_file() and ".agent-fabric" not in path.parts
    }
    assert committed, "the seeded instance should carry committed files"
    for path in committed:
        assert str(product.resolve()) not in path.read_text()


def test_non_pointer_seed_path_does_not_call_pointer_writer(tmp_path, monkeypatch):
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()

    def unexpected_pointer_write(*args, **kwargs):
        raise AssertionError("seed --no-pointer must not publish the pointer")

    monkeypatch.setattr(instance, "write_pointer", unexpected_pointer_write)
    result = instance.execute(
        "seed", product, instance_root, write_product_pointer=False
    )

    assert result["product_pointer"] is None
    assert not (instance_root / ".agent-fabric/product-root.json").exists()


def test_validate_summary_is_non_mutating_and_reports_seed_state(tmp_path):
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()

    result = run("validate", product, instance_root, "--summary")

    assert result.returncode == 0, result.stderr
    assert "instance mode=split desired-state=missing seeded=0 existing=0 missing=3" in result.stdout
    assert not (instance_root / "config" / "installation.json").exists()
    assert not (instance_root / ".agent-fabric" / "product-root.json").exists()


def test_the_pointer_is_rewritten_rather_than_seeded_once(tmp_path):
    """Relocating the product is always a re-run of the installer."""
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()
    seed(product, instance_root)

    relocated = tmp_path / "relocated"
    shutil.copytree(product, relocated)
    result = seed(relocated, instance_root)

    assert result["product_pointer"]["product_root"] == str(relocated.resolve())
    assert instance.read_pointer(instance_root)["product_root"] == str(relocated.resolve())
    # The committed desired state is untouched by the relocation.
    assert result["desired_state_state"] == "existing"


def test_the_pointer_directory_is_ignored_by_git():
    ignored = (ROOT / ".gitignore").read_text().splitlines()
    assert ".agent-fabric/" in ignored


def test_a_fresh_split_instance_ignores_its_pointer_without_help(tmp_path):
    """Ignore rules do not cross repository roots, so the rule ships with it.

    An independent instance repository has never heard of the product
    checkout's .gitignore. The pointer carries an absolute machine path, so its
    directory has to be self-ignoring wherever the instance happens to live.
    """
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()
    subprocess.run(
        ["git", "init", "-q"], cwd=instance_root, check=True, capture_output=True
    )

    seed(product, instance_root)

    pointer = instance_root / ".agent-fabric" / "product-root.json"
    assert (instance_root / ".agent-fabric" / ".gitignore").read_text() == "*\n"
    check = subprocess.run(
        ["git", "check-ignore", "-q", str(pointer)],
        cwd=instance_root,
        capture_output=True,
    )
    assert check.returncode == 0, "git does not ignore the pointer in a fresh instance"

    # And nothing in the pointer directory can reach the index.
    staged = subprocess.run(
        ["git", "add", "-A"], cwd=instance_root, capture_output=True, text=True
    )
    assert staged.returncode == 0, staged.stderr
    tracked = subprocess.run(
        ["git", "diff", "--cached", "--name-only"],
        cwd=instance_root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()
    assert not any(name.startswith(".agent-fabric/") for name in tracked), tracked
    assert "config/installation.json" in tracked


@pytest.mark.parametrize(
    "document",
    [
        {"schema_version": 2, "product_root": "/opt/provenant"},
        {"schema_version": True, "product_root": "/opt/provenant"},
        {"schema_version": 1, "product_root": "relative/product"},
        {"schema_version": 1, "product_root": "/opt/provenant", "extra": 1},
    ],
)
def test_an_invalid_pointer_is_rejected_rather_than_trusted(tmp_path, document):
    instance_root = tmp_path / "instance"
    (instance_root / ".agent-fabric").mkdir(parents=True)
    (instance_root / ".agent-fabric" / "product-root.json").write_text(
        json.dumps(document) + "\n"
    )

    with pytest.raises(instance.InstallError, match="product pointer is invalid"):
        instance.read_pointer(instance_root)


def test_a_fused_layout_seeds_no_template_over_itself(tmp_path):
    product = build_product(tmp_path)
    original = (product / "AGENTS.md").read_text()

    result = seed(product, product)

    assert {item["state"] for item in result["seeded"]} == {"existing"}
    assert (product / "AGENTS.md").read_text() == original


def test_a_symlinked_destination_never_redirects_a_seeded_write(tmp_path):
    """Staging and renaming means the destination path cannot be hijacked."""
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()
    outside = tmp_path / "outside.md"
    outside.write_text("do not touch\n")
    # A symlink standing where the seeder is about to write. `exists()` follows
    # it, so this is reported as existing rather than copied through.
    (instance_root / "AGENTS.md").symlink_to(outside)

    result = seed(product, instance_root)

    assert outside.read_text() == "do not touch\n"
    states = {item["path"]: item["state"] for item in result["seeded"]}
    assert states["AGENTS.md"] == "existing"


def test_a_symlinked_parent_directory_cannot_carry_a_write_out_of_the_instance(tmp_path):
    """Renaming is only as contained as the directory it happens in.

    Hardening, not a privilege boundary: swapping a directory inside the
    instance root already needs the user's own privileges. It is cheap and it
    makes the failure loud.
    """
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    # config/ is where the desired state lands; point it out of the instance.
    (instance_root / "config").symlink_to(outside, target_is_directory=True)

    result = run("seed", product, instance_root)

    assert result.returncode == 3
    assert "escapes the instance root" in result.stderr
    assert list(outside.iterdir()) == [], "nothing may be written through the swap"


def test_seeding_fails_with_a_clean_message_when_the_instance_is_readonly(tmp_path):
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir(mode=0o500)
    try:
        result = run("seed", product, instance_root)
    finally:
        instance_root.chmod(0o700)

    assert result.returncode == 3
    assert "conflicting:" in result.stderr
    assert "not writable" in result.stderr
    assert "Traceback" not in result.stderr


def test_show_reports_the_instance_without_writing_it(tmp_path):
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()

    result = run("show", product, instance_root)

    assert result.returncode == 0, result.stderr
    reported = json.loads(result.stdout)
    assert reported["desired_state_state"] == "missing"
    assert reported["desired_state"] is None
    assert list(instance_root.iterdir()) == []


def test_an_unreadable_desired_state_fails_rather_than_being_replaced(tmp_path):
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    (instance_root / "config").mkdir(parents=True)
    corrupt = instance_root / "config" / "installation.json"
    corrupt.write_text("{not json\n")

    result = run("seed", product, instance_root)

    assert result.returncode == 3
    assert "desired state is unreadable" in result.stderr
    assert corrupt.read_text() == "{not json\n"


def test_a_missing_product_template_is_a_conflict_not_a_silent_skip(tmp_path):
    product = build_product(tmp_path)
    (product / "config" / "model-routing.json").unlink()
    instance_root = tmp_path / "instance"
    instance_root.mkdir()

    result = run("seed", product, instance_root)

    assert result.returncode == 3
    assert "product template is missing: config/model-routing.json" in result.stderr


def test_the_repository_ships_a_valid_fused_desired_state():
    """This checkout is itself a fused instance, so its desired state must load."""
    document = instance.load_desired_state(ROOT)

    assert document is not None
    assert document["mode"] == "fused"
    assert document["product"]["version"] == instance.product_version(ROOT)


def test_install_harness_seeding_leaves_a_seeded_instance_untouched(tmp_path):
    """The installer step is a no-op against an instance that already has state."""
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()
    seed(product, instance_root)
    before = {
        path: path.read_bytes()
        for path in sorted(instance_root.rglob("*"))
        if path.is_file()
    }

    result = run("seed", product, instance_root, "--summary")

    assert result.returncode == 0, result.stderr
    assert "desired-state=existing seeded=0 existing=3" in result.stdout
    assert {
        path: path.read_bytes()
        for path in sorted(instance_root.rglob("*"))
        if path.is_file()
    } == before


def test_the_installer_seeds_a_scratch_instance_root(tmp_path):
    """`install-harness` honours AGENT_FABRIC_INSTANCE_ROOT for the instance side."""
    source = (ROOT / "scripts" / "install-harness").read_text()
    # The instance root is resolved once, on the contract #529 established.
    assert 'instance_root="${AGENT_FABRIC_INSTANCE_ROOT:-$HOME/.agents}"' in source
    assert source.count("instance_root=$") + source.count('instance_root="$') == 1, (
        "install-harness must resolve the instance root exactly once"
    )
    assert "scripts/instance_installation.py\" seed" in source

    product = tmp_path / "product"
    shutil.copytree(ROOT / "config", product / "config", symlinks=True)
    (product / "package.json").write_text(json.dumps({"version": "0.0.1"}) + "\n")
    (product / "AGENTS.md").write_text("# doctrine\n")
    (product / "config" / "installation.json").unlink()
    instance_root = tmp_path / "instance"
    instance_root.mkdir()

    result = seed(product, instance_root)

    assert result["desired_state"]["product"]["version"] == "0.0.1"
    assert result["mode"] == "split"


def test_the_standalone_cli_defaults_the_instance_root_to_the_instance_not_the_product(tmp_path):
    """Defaulting to the product checkout would reinstate the fused assumption.

    Run standalone, this would otherwise seed the product tree rather than the
    user's instance, which is exactly the confusion ADR 0019 removes.
    """
    home = tmp_path / "home"
    (home / ".agents").mkdir(parents=True)

    assert instance.default_instance_root({}) == Path.home() / ".agents"
    # AGENTS_HOME names the product root and must not select the instance.
    assert instance.default_instance_root(
        {"AGENTS_HOME": str(home / ".agents")}
    ) == Path.home() / ".agents"
    # An explicit instance root is the only environment override.
    assert instance.default_instance_root({
        "AGENT_FABRIC_INSTANCE_ROOT": str(tmp_path / "explicit"),
        "AGENTS_HOME": str(home / ".agents"),
    }) == tmp_path / "explicit"
    # A relative AGENTS_HOME is not an instance input and is ignored here.
    assert instance.default_instance_root({"AGENTS_HOME": "relative/product"}) == Path.home() / ".agents"
    # A relative explicit instance value would resolve against the caller's
    # working directory, so it is refused.
    with pytest.raises(instance.InstallError, match="AGENT_FABRIC_INSTANCE_ROOT must be an absolute path"):
        instance.default_instance_root({"AGENT_FABRIC_INSTANCE_ROOT": "relative/instance"})


def test_the_standalone_cli_seeds_the_environment_instance_root(tmp_path):
    product = build_product(tmp_path)
    instance_root = tmp_path / "env-instance"
    instance_root.mkdir()
    environment = {
        **os.environ,
        "AGENT_FABRIC_INSTANCE_ROOT": str(instance_root),
    }

    result = subprocess.run(
        [sys.executable, str(SCRIPT), "seed", "--product-root", str(product), "--summary"],
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert f"root={instance_root}" in result.stdout
    # The product tree is not the thing that got seeded.
    assert (instance_root / "config" / "installation.json").is_file()
    assert not (product / "config" / "installation.json").exists()


def test_the_standalone_cli_refuses_a_relative_environment_instance_root(tmp_path):
    """The refusal must arrive through the CLI contract, not a traceback.

    `default_instance_root` raising outside the handler would surface a stack
    trace with exit 1 instead of the `conflicting:` exit-3 contract every other
    refusal uses.
    """
    product = build_product(tmp_path)
    environment = {**os.environ, "AGENT_FABRIC_INSTANCE_ROOT": "relative/instance"}

    result = subprocess.run(
        [sys.executable, str(SCRIPT), "seed", "--product-root", str(product)],
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 3, result.stderr
    assert "conflicting: AGENT_FABRIC_INSTANCE_ROOT must be an absolute path" in result.stderr
    assert "Traceback" not in result.stderr


def test_a_symlinked_pointer_directory_is_refused_by_the_seeder(tmp_path):
    product = build_product(tmp_path)
    instance_root = tmp_path / "instance"
    instance_root.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    (instance_root / ".agent-fabric").symlink_to(outside, target_is_directory=True)

    result = run("seed", product, instance_root)

    assert result.returncode == 3
    assert "escapes the instance root" in result.stderr
    assert "Traceback" not in result.stderr
    assert list(outside.iterdir()) == []
