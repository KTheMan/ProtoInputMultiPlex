"""Missing Save/Load only; no keyboard/UIA/worker prefix replay. ROOT READY only."""
import argparse
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import struct


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def strict_equal(actual, expected):
    assert type(actual) is type(expected), (type(actual).__name__, type(expected).__name__)
    if isinstance(actual, dict):
        assert actual.keys() == expected.keys(), "Profile object keys differ"
        for key in actual:
            strict_equal(actual[key], expected[key])
    elif isinstance(actual, list):
        assert len(actual) == len(expected), "Profile list lengths differ"
        for a, b in zip(actual, expected):
            strict_equal(a, b)
    else:
        assert actual == expected, (actual, expected)


def equivalent(actual, expected):
    """Only B speed_x has an f32 wire/JSON spelling difference; all else exact."""
    a, b = copy.deepcopy(actual), copy.deepcopy(expected)
    av = a["seats"][1]["cursor"].pop("speed_x")
    bv = b["seats"][1]["cursor"].pop("speed_x")
    assert type(av) in (int, float) and type(bv) in (int, float), "B speed must be numeric, never bool"
    assert struct.pack("<f", av) == struct.pack("<f", bv), (av, bv)
    strict_equal(a, b)


def module(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready", action="store_true")
    for name in ("executable", "x86-executable", "runtime", "producer", "from-run"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--producer-sha256", required=True)
    args = parser.parse_args()
    source = module("keyboard_prefix_guard", "test-keyboard-configuration-ui.py")
    assert digest(Path(__file__).with_name("test-keyboard-configuration-ui.py")) == "7ebc3f50321c468de50b73b2fa8bacae7c85c8ead4977fc34843fb2a714a347e"
    source.freeze(args)
    repo = Path(__file__).resolve().parent.parent
    prior = repo / "runtime/acceptance-graph-keyboard-configuration-20261015-062722"
    assert args.from_run.resolve() == prior
    for name, value in {
        "evidence.json": "e948224a515d0be6ca0de2ae40b1c41dfba4946634fe2296bf108977717801b8",
        "independent-review.json": "8b0e71d6e9c1844144b4a3392f8611fcfea3912f7a19cf2a1be49789a3e5bc43",
        "ui.profile.json": "bb41fcaeb623cf31992265b2bf868e8fa225f88b550ab0d2775a45fe04c71d85",
    }.items():
        assert digest(prior / name) == value
    prefix = json.loads((prior / "evidence.json").read_text(encoding="utf-8"))
    producer = json.loads(args.producer.read_text(encoding="utf-8"))
    assert set(prefix["build_sha256"].values()) == set(producer["build_sha256"].values())
    profile = json.loads((prior / "ui.profile.json").read_text(encoding="utf-8"))["profile"]
    assert struct.pack("<f", profile["seats"][1]["cursor"]["speed_x"]) == struct.pack("<f", 1.3)
    preferences = json.loads((prior / "owned preferences.json").read_text(encoding="utf-8"))
    lifecycle = module("keyboard_persistence_lifecycle", "test-app-lifecycle-ui.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    g = Base(args, "keyboard-persistence", offline=True, count=0)
    output = g.run / "fresh keyboard profile.json"
    assert not output.exists()
    saved_bytes = None
    before_save_status = None
    try:
        provenance = g.run / "runner-source"
        provenance.mkdir()
        shutil.copy2(Path(__file__), provenance / Path(__file__).name)
        shutil.copy2(repo / "runtime/keyboard-persistence-acceptance-plan.md", provenance / "plan-at-launch.md")
        steps = [ui.step("settings", "Settings"),
                 ui.step("users", "Settings category Users & devices"),
                 ui.step("profiles", "Profiles & backend"),
                 ui.step("save", "Save profile", {"profile_path": output}),
                 ui.step("reload", "Load profile")]
        g.evidence.update(retained_prefix=str(prior), producer=str(args.producer),
                          producer_sha256=digest(args.producer),
                          scope="Five pointer observations; stopped DLL-free Save/Load only. Seed is explicit continuation setup, not prior persistence proof.")

        def verify(name, sample):
            nonlocal saved_bytes, before_save_status
            snap = sample["profile_state"]
            equivalent(snap["profile"], profile)
            assert ui.read_json(g.preferences_path) == preferences
            assert not sample["state"], "Persistence-only tail unexpectedly has routing state"
            if name == "profiles":
                assert not output.exists(), "Fresh destination was created before real Save"
                before_save_status = snap["status"]
                g.evidence["before_save"] = {"destination": str(output), "absent": True,
                                             "status": before_save_status}
            if name == "save":
                if snap["status"] == before_save_status:
                    return False  # Receipt precedes synchronous Save completion; preserve original deadline.
                assert snap["status"] == f"Saved {output}. No previous file existed to back up.", snap["status"]
                assert snap["active_path"] == str(output)
                if not output.exists() or not output.stat().st_size:
                    return False
                data = output.read_bytes()
                equivalent(json.loads(data), profile)
                assert not list(g.run.glob("*.tmp.*")), "Committed Save still has a temporary file"
                saved_bytes = data
                g.evidence["committed_save"] = {"path": str(output), "sha256": digest(output),
                                                "bytes": len(data), "status": snap["status"]}
            if name == "reload":
                if snap["status"].startswith(f"Saved {output}."):
                    return False
                assert snap["status"] == "Profile loaded. Verify target PIDs before attaching.", snap["status"]
                assert snap["active_path"] == str(output) and output.read_bytes() == saved_bytes
                equivalent(json.loads(saved_bytes), snap["profile"])
                assert not list(g.run.glob("*.tmp.*"))
                shot = g.screenshot("saved-and-reloaded")
                if not shot:
                    return False
                sample["owned_original"] = shot
            return True

        def after(gate):
            gate.expected_exit = True
            assert geometry.user32.PostMessageW(gate.main_hwnd(), 0x10, 0, 0)
            gate.gui.wait(timeout=10)
            assert gate.gui.returncode == 0 and not hotkeys.lifecycle_window(gate.gui.pid)
            return True

        g.execute(profile, preferences, steps, verify, after_controls=after)
        g.evidence["result"] = "PENDING INDEPENDENT PERSISTENCE/PIXEL REVIEW"
    finally:
        g.close()
        lifecycle.remove_startup_key(g.startup_key)
        g.save()


if __name__ == "__main__":
    main()
