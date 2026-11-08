"""CI-only Git/source integration checks. No compiler or product code is run."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
VENDOR = ROOT/"vendor/ProtoInput"
PIN = "dd29bf8d22349605d0e1950c1c5dc4893d72670e"
PATCHES = (
    "patches/0001-backend.patch",
    "patches/lifecycle/0004-clean-restart.patch",
    "patches/keyboard-state/0005-consumed-keyboard-state.patch",
    "patches/lifecycle/0006-checked-message-filter-setup.patch",
    "patches/stdio/0007-preserve-target-stdio.patch",
)
ADDED = tuple("vendor/ProtoInput/src/ProtoInput/ProtoInputHooks/"+name for name in (
    "MuxCursorObserverBridge.h", "MuxLifecycleNative.h", "pipeinclude/MuxLifecycleProtocol.h",
    "MuxKeyboardStateBridge.h", "pipeinclude/MuxSetupAckProtocol.h",
))


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def git(*args):
    return subprocess.check_output(["git", *args], cwd=VENDOR)


def unchanged(path, expected, label):
    if digest(path) != expected:
        raise ValueError(label+" changed during source inventory")


def main():
    # This script writes prepared source/probe files only in the disposable
    # hosted checkout; it is deliberately unavailable in an ordinary local tree.
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_OS") != "Windows":
        raise ValueError("Use the hosted Windows packaging source-check lane")
    workspace = Path(os.environ["GITHUB_WORKSPACE"]).resolve()
    if ROOT != workspace or not VENDOR.resolve().is_relative_to(workspace):
        raise ValueError("Source-check paths must belong to the hosted checkout")
    if not (VENDOR/".git").exists():
        raise ValueError("Checkout must contain the actual pinned native submodule")
    if git("rev-parse", "HEAD").decode().strip() != PIN:
        raise ValueError("Native submodule is not pinned")
    if git("status", "--porcelain", "--untracked-files=all").strip():
        raise ValueError("Native source preparation requires a fresh clean checkout")
    spec = importlib.util.spec_from_file_location("portable", ROOT/"scripts/package-portable.py")
    portable = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(portable)
    if portable.PIN != PIN or tuple(portable.BACKEND_PATCH_SEQUENCE) != PATCHES:
        raise ValueError("Source-check recipe differs from approved packaging recipe")
    for number, name in enumerate(PATCHES):
        flags = ["--ignore-space-change"]
        if number:
            flags.append("--whitespace=nowarn")
        git("apply", *flags, str(ROOT/name))

    # The inventory independently derives final blobs by replaying all patches
    # into its temporary index/object store. The working index must stay intact.
    raw_index = Path(git("rev-parse", "--git-path", "index").decode().strip())
    index = raw_index if raw_index.is_absolute() else VENDOR/raw_index
    index_hash = digest(index)
    before = {name: digest(ROOT/name) for name in ADDED}
    public_oldfile = ROOT/"crates/mux-application-sdk/Cargo.toml"
    old_hash = digest(public_oldfile)
    names = portable.source_files()
    if not set(ADDED).issubset(names):
        raise ValueError("Final patch-created header closure was omitted")
    unchanged(index, index_hash, "Native working index")
    for name, expected in before.items():
        unchanged(ROOT/name, expected, "Prepared source")

    probe = ROOT/"scripts/private-acceptance/packaging-source-probe.json"
    if probe.exists():
        raise ValueError("Controlled source probe path must be fresh")
    probe.parent.mkdir(parents=True, exist_ok=True)
    probe.write_text('{"purpose":"public-nonsecret-source-admission-probe"}\n', encoding="utf-8")
    probe_hash = digest(probe)
    rejected = False
    try:
        portable.source_files()
    except ValueError as error:
        if "Uncommitted source candidates" not in str(error):
            raise
        rejected = True
    if not rejected:
        raise ValueError("Untracked source probe was admitted")
    unchanged(probe, probe_hash, "Controlled probe")
    unchanged(public_oldfile, old_hash, "Existing public source")
    unchanged(index, index_hash, "Native working index")
    print(json.dumps({"schema": "packaging-source-check.v1", "status": "PASS",
                      "native_pin": PIN, "patch_stages": len(PATCHES),
                      "admitted_final_headers": len(ADDED), "source_files": len(names),
                      "private_probe_rejected": rejected, "working_index_unchanged": True,
                      "scope": "Git/source only; no release build or runtime qualification"}))


if __name__ == "__main__":
    main()
