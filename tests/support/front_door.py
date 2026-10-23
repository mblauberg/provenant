"""The Rust front door used by CLI integration tests."""

from functools import lru_cache
import os
from pathlib import Path

from lib.front_door_build import build, source_digest

ROOT = Path(__file__).resolve().parents[2]


@lru_cache(maxsize=1)
def front_door_binary() -> Path:
    configured = os.environ.get("PROVENANT_FRONT_DOOR_BIN")
    if configured:
        binary = Path(configured).resolve()
        if not binary.is_file():
            raise FileNotFoundError(f"PROVENANT_FRONT_DOOR_BIN does not name a binary: {binary}")
        return binary
    crate = ROOT / "tools/front-door"
    target = Path(os.environ.get("CARGO_TARGET_DIR", crate / "target"))
    return build(crate, target, source_digest(crate))
