"""Freeze/check the isolated UX candidate; no product GUI or routing launch."""
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

root = Path(__file__).resolve().parent.parent
target = root.parent / "ProtoInputMultiPlex-ux-target"
run = root / "runtime" / ("producer-feature-workflows-ux-" + datetime.datetime.now().strftime("%Y%m%d-%H%M%S"))
run.mkdir(parents=True)
sha = lambda path: hashlib.sha256(Path(path).read_bytes()).hexdigest()
def sources():
    paths = list((root / "crates").rglob("*.rs")) + list((root / "crates").rglob("Cargo.toml"))
    paths += [root / "Cargo.toml", root / "Cargo.lock", Path(__file__)]
    return {path.relative_to(root).as_posix(): sha(path) for path in sorted(paths)}
native = json.loads((root / "runtime/native-baseline.json").read_text(encoding="utf-8"))
assert all(sha(root / "runtime" / name) == value for name, value in native["runtime_sha256"].items()), "Native baseline drift"
record = {"result": "RUNNING", "source_before_sha256": sources(), "checks": [], "target_directory": str(target), "native_baseline": native}
def save():
    (run / "verification.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
env = dict(os.environ, CARGO_TARGET_DIR=str(target), CARGO_BUILD_JOBS="2")
commands = [
    ["cargo", "fmt", "--all", "--check"],
    ["cargo", "clippy", "--config", 'build.rustc-wrapper=""', "--workspace", "--all-targets", "--", "-D", "warnings"],
    ["cargo", "test", "--config", 'build.rustc-wrapper=""', "--workspace"],
    ["cargo", "build", "--config", 'build.rustc-wrapper=""', "--workspace"],
    ["cargo", "build", "--config", 'build.rustc-wrapper=""', "--workspace", "--target", "i686-pc-windows-msvc"],
]
save()
print(run, flush=True)
for index, command in enumerate(commands, 1):
    record["active_command"] = command
    save()
    log = run / f"{index}.log"
    with log.open("w", encoding="utf-8") as output:
        result = subprocess.run(command, cwd=root, env=env, stdout=output, stderr=subprocess.STDOUT)
    record["checks"].append({"command": command, "exit_code": result.returncode, "log": str(log), "sha256": sha(log)})
    if index == 3:
        record["workspace_tests"] = sum(map(int, re.findall(r"test result: ok\. (\d+) passed", log.read_text(encoding="utf-8"))))
    print(json.dumps(record["checks"][-1]), flush=True)
    save()
    if result.returncode:
        break
record["source_after_sha256"] = sources()
passed = len(record["checks"]) == 5 and all(check["exit_code"] == 0 for check in record["checks"]) and record["source_before_sha256"] == record["source_after_sha256"]
if passed:
    record["build_sha256"] = {}
    for label, path in (("x64", target / "debug/protoinputmultiplex.exe"), ("i686", target / "i686-pc-windows-msvc/debug/protoinputmultiplex.exe")):
        frozen = run / (label + ".exe")
        shutil.copyfile(path, frozen)
        record["build_sha256"][label] = sha(frozen)
    assert all(sha(root / "runtime" / name) == value for name, value in native["runtime_sha256"].items()), "Native drift during checks"
    record["runtime_sha256"] = native["runtime_sha256"]
record["result"] = "PASS" if passed else "FAIL"
record.pop("active_command", None)
save()
print(json.dumps({key: record[key] for key in ("result", "checks")}), flush=True)
raise SystemExit(0 if passed else 1)
