"""Shared helpers for the cf_dispatch test modules."""

import os
import stat
import textwrap
from pathlib import Path

PRODUCT_ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = PRODUCT_ROOT / "skills" / "orchestrate" / "scripts"
SCRIPT = SCRIPTS / "cf_dispatch.py"
RUN_DIR_SCRIPT = SCRIPTS / "run_dir_init.sh"


def write_executable(path, body):
    path.write_text(textwrap.dedent(body).lstrip(), encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def fabric_free_env():
    # Keep the evals on this checkout's fused fixtures rather than an inherited
    # developer instance or the operator's default ~/.agents instance.
    env = {
        key: value
        for key, value in os.environ.items()
        if key != "AGENTS_HOME" and not key.startswith("AGENT_FABRIC_")
    }
    env["AGENT_FABRIC_PRODUCT_ROOT"] = str(PRODUCT_ROOT)
    # conftest pins a per-test instance root; keep it rather than the live checkout.
    env["AGENT_FABRIC_INSTANCE_ROOT"] = os.environ["AGENT_FABRIC_INSTANCE_ROOT"]
    env["PROVENANT_NO_OS_CONFINEMENT"] = "1"
    return env
