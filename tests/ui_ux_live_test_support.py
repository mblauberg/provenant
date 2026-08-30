import json
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SKILL_SCRIPTS = ROOT / "skills" / "ui-ux-design" / "scripts"
SCRIPTS = ROOT / "runtime" / "ui-live"
LIVE = SKILL_SCRIPTS / "live.mjs"
SERVER_WRAPPER = SKILL_SCRIPTS / "live-server.mjs"
INJECT = SCRIPTS / "live-inject.mjs"
SERVER = SCRIPTS / "live-server.mjs"
WRAP = SCRIPTS / "live-wrap.mjs"
ACCEPT = SCRIPTS / "live-accept.mjs"
RESUME = SCRIPTS / "live-resume.mjs"
STATUS = SCRIPTS / "live-status.mjs"
TOKEN = "11111111-1111-4111-8111-111111111111"


def write_live_config(project: Path, files: list[str]) -> None:
    config_dir = project / ".impeccable" / "live"
    config_dir.mkdir(parents=True, exist_ok=True)
    (config_dir / "config.json").write_text(
        json.dumps(
            {
                "files": files,
                "insertBefore": "</body>",
                "commentSyntax": "html",
            }
        )
    )
