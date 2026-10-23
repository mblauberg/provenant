"""Run source lane contracts with isolated files and explicit fixture inputs."""
import importlib.util
import json
import os
import shutil
from pathlib import Path
import sys
import tempfile

root, unit = Path(sys.argv[1]), sys.argv[2]
spec = importlib.util.spec_from_file_location("lane_exit_oracle", root / "skills/orchestrate/scripts/lane_exit.py")
lane_exit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lane_exit)
results = []
for case in json.load(sys.stdin):
    if unit == "restore":
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / "observer.json").write_text(json.dumps(case), encoding="utf-8")
            observer = lane_exit.settle_observer({"adapter":"claude", "model":"fixture", "run_dir":directory}, {}, path / "events", path / "stderr")
            results.append({"wire":json.dumps(observer.result(), ensure_ascii=False, separators=(",",":")), "text_size":observer.text_size})
    elif unit == "launch":
        spec = importlib.util.spec_from_file_location("lane_launch_oracle", root / "skills/orchestrate/scripts/lane_launch.py")
        launch = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(launch)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            shutil.copyfile(root / "tools/front-door/crates/workflow/tests/fake_lane_front_door.py", path / "provenant")
            (path / "provenant").chmod(0o700)
            payload = dict(case, attempt_dir=directory)
            (path / "fixture.json").write_text(json.dumps(payload), encoding="utf-8")
            callbacks = []
            def started(provider):
                callbacks.append(["started", provider.pid])
                if case.get("bookkeeping_error"):
                    raise OSError("full")
            def progress(at):
                callbacks.append(["progress", at])
                if case.get("bookkeeping_error"):
                    raise OSError("full")
            env = {"PATH":os.environ.get("PATH", ""), "PROVENANT_BIN_DIR":directory,
                   "PORT_FIXTURE_PATH":str(path / "fixture.json")}
            try:
                outcome = launch.run({"attempt_dir":directory}, env, on_start=started,
                                     on_progress=progress if case.get("progress", True) else None)
                if outcome.unsupported:
                    result = {"unsupported":True, "warning":outcome.warning}
                else:
                    provider = None if outcome.provider is None else {"pid":outcome.provider.pid,"returncode":outcome.provider.returncode}
                    result = {"unsupported":False,"record":outcome.record,"fields":outcome.fields,
                              "provider":provider,"started_at":outcome.started_at,"warning":outcome.warning}
                results.append({"wire":json.dumps(result,ensure_ascii=False,separators=(",",":")),"callbacks":callbacks})
            except (ValueError, KeyError) as error:
                results.append({"error":str(error),"callbacks":callbacks})
json.dump(results, sys.stdout)
