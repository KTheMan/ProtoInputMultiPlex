"""Prepared finite stopped four-form layout gate. Requires explicit frozen READY."""
import argparse
import copy
import hashlib
import importlib.util
from pathlib import Path
import shutil
import time


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def workflow(args):
    lifecycle = module("forms_lifecycle", "test-app-lifecycle-ui.py")
    fixtures = module("forms_fixture", "test-per-user-tools-ui.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()

    class Gate(Base):
        def __init__(self):
            super().__init__(args, "per-user-forms", offline=True, count=0)
            self.frames = {}
            self.requested = {}

        def frame(self, name, size):
            if name in self.frames:
                return True
            hwnd = self.main_hwnd()
            if not hwnd:
                return False
            if name not in self.requested:
                geometry.resize_owned_client(hwnd, size)
                self.requested[name] = time.time()
                return False
            current = geometry.geometry(hwnd)
            if any(abs(a-b) > 2 for a, b in zip(current["client_logical"], size)):
                return False
            shot = geometry.copy_owned_product_frame(
                self.run/"ui.png", self.run/(name+".png"), hwnd, self.requested[name])
            if not shot:
                return False
            shot["geometry"] = current
            self.frames[name] = shot
            return True

        def pair(self, name):
            for suffix, size in (("normal", [540, 520]), ("minimum", [320, 360])):
                if not self.frame(name+"-"+suffix, size):
                    return False
            return True

    g = Gate()
    profile = fixtures.profile_fixture()
    for i, seat in enumerate(profile["seats"]):
        seat["id"] = f"forms-owned-{i}"
        seat["cursor"].update(speed_x=1.25+i, speed_y=1.5+i,
                              wheel_scale=1.5+i, motion_curve="linear",
                              horizontal_wheel="ignore")
    prefs = {"version": 1, "appearance_mode": "dark", "zoom_percent": 100,
             "launch_minimized": False, "minimize_to_tray": False,
             "close_to_tray": False, "launch_at_login": False, "shortcuts": []}
    step = ui.step
    steps = [step("select-b", "Select user 2"), step("configure-b", "Configure user"),
             step("input-b", "Input"), step("motion-b", "Motion"),
             step("gentle-b", "Gentle response"), step("buttons-b", "Buttons"),
             step("wheel-b", "Wheel"), step("preserve-b", "Preserve horizontal wheel"),
             step("apply-disabled", "Apply cursor settings", disabled=True),
             step("home", "Back to Home"), step("select-a", "Select user 1"),
             step("configure-a", "Configure user"), step("motion-a", "Motion")]
    assert len(steps) == 13 and not any(s.get("fields") for s in steps)
    if args.from_run:
        prior = ui.read_json(args.from_run/"evidence.json")
        assert prior["result"] == "PENDING INDEPENDENT VISUAL REVIEW"
        assert prior["control_and_profile_result"] == "PASS" and prior["owned_processes_closed"]
        assert len(prior["samples"]) == (8 if args.buttons_only else 13)
        required_hashes = {
            "f63908a6dab5071520cf8d455a5740d629bda325fba9da315d94f6a0491ed96c",
            "85a90966e1259effa6da16e17fb67193d783dcfdb748f39df5398249c63ae821"
        } if args.buttons_only else {
            "32c882ee38738ec79f157d80b7aeb6daee2f6a9eb246bd9c7e32c9783d912c70",
            "eaccb8521f3eda218feb61b32de55554172ef91bac61ab62c530e0e422c1e578"}
        assert set(prior["build_sha256"].values()) == required_hashes
        profile = copy.deepcopy(ui.read_json(args.from_run/"final-profile.json"))
        assert profile["seats"][1]["cursor"]["motion_curve"] == "gentle"
        assert profile["seats"][1]["cursor"]["horizontal_wheel"] == "preserve"
        steps = [step("select-b", "Select user 2"), step("configure-b", "Configure user"),
                 step("motion-b", "Motion"), step("buttons-b", "Buttons"),
                 step("wheel-b", "Wheel"), step("wheel-hold", "Wheel"),
                 step("preserve-b", "Preserve horizontal wheel"), step("home", "Back to Home")]
        assert len(steps) == 8 and not any(s.get("fields") for s in steps)
        g.evidence["retained_prefix"] = {
            "path": str(args.from_run/"evidence.json"),
            "sha256": hashlib.sha256((args.from_run/"evidence.json").read_bytes()).hexdigest(),
            "scope": "13 control/profile observations retained; eight focused reveal observations with same-value Preserve only for lower-body reveal; no business-choice/Apply revalidation"}
        if args.buttons_only:
            steps = [step("select-b", "Select user 2"), step("configure-b", "Configure user"),
                     step("buttons-b", "Buttons"), step("home", "Back to Home")]
            g.evidence["retained_prefix"]["scope"] = "13 original plus8 continuation receipts retained; four-observation Buttons-only geometry tail, exact prior finalprofile, no business-choice replay"
    expected = None
    transitions = {}

    def verify(name, sample):
        nonlocal expected
        current = g.snapshot()["profile"]
        if expected is None:
            expected = copy.deepcopy(current)
            if args.from_run:
                assert current == profile, "Continuation changed accepted exact seed"
            assert current["seats"][0]["cursor"]["speed_x"] == 1.25
            assert current["seats"][1]["cursor"]["speed_x"] == 2.25
        edits = {"gentle-b": ("motion_curve", "gentle"),
                 "preserve-b": ("horizontal_wheel", "preserve")}
        if name in edits:
            if name not in transitions:
                after = copy.deepcopy(expected)
                key, value = edits[name]
                after["seats"][1]["cursor"][key] = value
                transitions[name] = (copy.deepcopy(expected), after)
            before, after = transitions[name]
            if current == before and current != after:
                return False
            assert current == after, "Choice changed unrelated profile/user fields"
            expected = copy.deepcopy(after)
        else:
            assert current == expected, "Navigation/geometry/disabled Apply mutated profile"
        assert not (g.run/"ui.state.json").exists(), "Stopped gate unexpectedly created worker"
        sample["complete_profile_verified"] = True
        frame_names = {"input-b": "input-b", "motion-b": "motion-upper-b",
                       "gentle-b": "motion-response-b", "buttons-b": "buttons-b",
                       "wheel-b": "wheel-upper-b", "preserve-b": "wheel-policy-b"}
        if args.from_run and not args.buttons_only:
            frame_names["configure-b"] = "input-b"
        if name in frame_names and not g.pair(frame_names[name]):
            return False
        if name == "motion-a" and not g.frame("motion-a-unchanged", [540, 520]):
            return False
        return True

    def after(gate):
        ui.put(gate.run/"final-profile.json", gate.snapshot()["profile"])
        gate.expected_exit = True
        assert geometry.user32.PostMessageW(gate.main_hwnd(), 0x10, 0, 0)
        gate.gui.wait(timeout=10)
        assert gate.gui.returncode == 0 and not hotkeys.lifecycle_window(gate.gui.pid)
        return True

    try:
        provenance = g.run/"runner-source"
        provenance.mkdir()
        for path in (Path(__file__), Path(__file__).with_name("test-per-user-tools-ui.py")):
            shutil.copy2(path, provenance/path.name)
        g.evidence["scope"] = "Stopped four-form layout/in-memory selected-user choices; no worker/Apply ACK/persistence/algorithm claim; i686 copied unused"
        g.execute(profile, prefs, steps, verify, lambda gate: gate.main_hwnd() is not None, after)
        g.evidence.update(captures=g.frames, control_and_profile_result="PASS",
                          result="PENDING INDEPENDENT VISUAL REVIEW")
        g.save()
        return g.run
    finally:
        g.close()
        lifecycle.remove_startup_key(g.startup_key)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready", action="store_true")
    parser.add_argument("--from-run", type=Path)
    parser.add_argument("--buttons-only", action="store_true")
    for name in ("executable", "x86-executable", "runtime"):
        parser.add_argument("--"+name, type=Path, required=True)
    args = parser.parse_args()
    assert args.ready, "Await frozen images and exclusive slot"
    assert not args.buttons_only or args.from_run, "Buttons-only consumes exact accepted continuation"
    workflow(args)


if __name__ == "__main__":
    main()
