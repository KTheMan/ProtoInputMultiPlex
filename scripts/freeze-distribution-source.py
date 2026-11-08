"""Copy current tracked/untracked source, including patched submodule, into a fresh Git snapshot."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess


def names(root):
    return subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=root).decode("utf-8").split("\0")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def source_paths(root):
    vendor = root/"vendor/ProtoInput"
    paths = set(p for p in names(root) if p and (root/p).is_file())
    paths.update("vendor/ProtoInput/"+p for p in names(vendor) if p and (vendor/p).is_file())
    return sorted(p for p in paths if "__pycache__" not in p and not p.endswith(".pyc") and p != ".gitmodules")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    output = args.output.resolve()
    if output.exists() or output == root or root in output.parents:
        parser.error("output must be fresh and outside the source checkout")
    paths = source_paths(root)
    vendor = root/"vendor/ProtoInput"
    before = {p: digest(root/p) for p in paths}
    output.mkdir(parents=True)
    for name in paths:
        destination = output/name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(root/name, destination)
    for _ in range(4):
        after = {p: digest(root/p) for p in paths}
        changed = [p for p in paths if before[p] != after[p]]
        if not changed:
            break
        for name in changed:
            shutil.copyfile(root/name, output/name)
        before = after
    else:
        raise RuntimeError("source kept changing during snapshot; retain failed snapshot and choose a fresh output")
    if before != {p: digest(output/p) for p in paths}:
        raise RuntimeError("snapshot copy does not match frozen source")
    if paths != source_paths(root):
        raise RuntimeError("source inventory changed during snapshot; choose a fresh output")
    if (vendor/".git").exists():
        native_pin = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=vendor, text=True).strip()
    else:
        native_pin = json.loads((root/"source-baseline.json").read_text(encoding="utf-8"))["protoinput_commit"]
    record = {"schema_version": 1, "source_checkout": str(root), "files": before,
              "protoinput_commit": native_pin,
              "method": "Current tracked and untracked files; vendor flattened with current compatibility patches; no build artifacts"}
    (output/"source-baseline.json").write_text(json.dumps(record, indent=2)+"\n", encoding="utf-8")
    subprocess.run(["git", "init", "--quiet", str(output)], check=True)
    subprocess.run(["git", "config", "core.autocrlf", "false"], cwd=output, check=True)
    # Formerly tracked submodule inputs can match upstream ignore patterns after
    # flattening. Force only the inventoried files, never newly generated output.
    subprocess.run(["git", "add", "-f", "--pathspec-from-file=-", "--pathspec-file-nul"],
                   input="\0".join(paths + ["source-baseline.json"]).encode("utf-8") + b"\0",
                   cwd=output, check=True)
    subprocess.run(["git", "-c", "user.name=Distribution Snapshot", "-c", "user.email=distribution@localhost",
                    "-c", "core.autocrlf=false", "-c", "commit.gpgsign=false",
                    "-c", "maintenance.auto=false", "-c", "gc.auto=0",
                    "commit", "--quiet", "-m", "Freeze current distribution source"], cwd=output, check=True)
    print(json.dumps({"snapshot": str(output), "files": len(paths), "baseline_sha256": digest(output/"source-baseline.json")}))


if __name__ == "__main__":
    main()
