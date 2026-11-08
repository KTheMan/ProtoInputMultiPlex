"""Finite stopped Draw/Magnifier presentation gate; launch only after frozen READY."""
import argparse
import copy
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
    lifecycle = module("hierarchy_lifecycle", "test-app-lifecycle-ui.py")
    fixtures = module("hierarchy_fixtures", "test-per-user-tools-ui.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()

    class Gate(Base):
        def __init__(self):
            super().__init__(args, "draw-magnifier-hierarchy", offline=True, count=0)
            self.frames, self.requested = {}, {}

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
            if any(abs(a-b) > 2 for a,b in zip(current["client_logical"], size)):
                return False
            shot = geometry.copy_owned_product_frame(
                self.run/"ui.png", self.run/(name+".png"), hwnd, self.requested[name])
            if not shot:
                return False
            shot["geometry"] = current
            self.frames[name] = shot
            return True

    g = Gate()
    profile = fixtures.profile_fixture()
    profile["seats"][0]["name"] = "Owned Alpha"
    profile["seats"][1]["name"] = "Owned Beta with a bounded long selected-user name"
    profile["seats"][1]["annotation"].update(tool="brush", clear_key=117)
    prefs = {"version":1,"appearance_mode":"dark","zoom_percent":100,
        "launch_minimized":False,"minimize_to_tray":False,"close_to_tray":False,
        "launch_at_login":False,"shortcuts":[]}
    step = ui.step
    # Repeated tab/disclosure/disabled targets are sampling guards: the next target
    # cannot scroll the page to a different body section before the saved frame.
    steps = [step("select-b","Select user 2"), step("configure-b","Configure user"),
        step("draw","Draw"), step("draw-guard","Draw"),
        step("layer-open","Layer details"), step("layer-close","Layer details"),
        step("draw-actions","Apply annotation settings",disabled=True),
        step("clear-user-disabled","Clear selected user's ink",disabled=True),
        step("clear-all-disabled","Clear all users' ink",disabled=True),
        step("draw-invalid","Apply annotation settings",{"annotation_clear_key":"not-a-key"},disabled=True),
        step("draw-invalid-guard","Apply annotation settings",disabled=True),
        step("draw-repair","Draw",{"annotation_clear_key":"F6"}),
        step("magnifier","Magnifier"), step("magnifier-guard","Magnifier"),
        step("capture-open","Capture details"), step("capture-close","Capture details"),
        step("magnifier-actions","Apply magnifier settings",disabled=True),
        step("refresh-disabled","Refresh magnifier",disabled=True),
        step("png-disabled","Save magnified PNG",disabled=True),
        step("magnifier-invalid","Apply magnifier settings",{"magnifier_monitor_id":"owned-missing-display"},disabled=True),
        step("magnifier-invalid-guard","Apply magnifier settings",disabled=True),
        step("magnifier-repair","Magnifier",{"magnifier_monitor_id":"none"}),
        step("home-b","Back to Home"), step("select-a","Select user 1"),
        step("configure-a","Configure user"), step("draw-a","Draw"),
        step("magnifier-a","Magnifier"), step("home-a","Back to Home")]
    assert len(steps) == 28
    assert not any({"annotation_user","magnifier_user","selected_user"}&set(s.get("fields",{})) for s in steps)
    frames = {"draw":("draw-normal",[540,520]),
        "layer-open":("draw-layer-open",[540,520]),
        "draw-actions":("draw-minimum-actions",[320,360]),
        "draw-invalid":("draw-error-details-closed",[540,520]),
        "magnifier":("magnifier-normal",[540,520]),
        "capture-open":("magnifier-capture-open",[540,520]),
        "magnifier-actions":("magnifier-minimum-actions",[320,360]),
        "magnifier-invalid":("magnifier-error-details-closed",[540,520])}
    baseline = None
    preferences_bytes = None

    def verify(name, sample):
        nonlocal baseline, preferences_bytes
        gate = g
        current = gate.snapshot()["profile"]
        if baseline is None:
            baseline = copy.deepcopy(current)
            preferences_bytes = gate.preferences_path.read_bytes()
            assert ui.read_json(gate.preferences_path) == prefs
        assert current == baseline, "Presentation/disclosure/invalid draft changed committed profile"
        assert gate.preferences_path.read_bytes() == preferences_bytes, "Presentation changed preferences"
        assert not (gate.run/"ui.state.json").exists(), "Stopped gate unexpectedly started worker"
        for stem in ("annotation","magnifier"):
            ack = gate.run/("ui."+stem+".acks.jsonl")
            assert not ack.exists() or not ack.read_text(encoding="utf-8").strip(), "Disclosure issued worker command"
        sample["complete_profile_verified"] = True
        if name in frames and not gate.frame(*frames[name]):
            return False
        return True

    def after(gate):
        ui.put(gate.run/"final-profile.json",gate.snapshot()["profile"])
        gate.expected_exit = True
        assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0)
        gate.gui.wait(timeout=10)
        assert gate.gui.returncode == 0 and not hotkeys.lifecycle_window(gate.gui.pid)
        return True

    try:
        source = g.run/"runner-source"
        source.mkdir()
        for path in (Path(__file__),Path(__file__).with_name("test-per-user-tools-ui.py")):
            shutil.copy2(path,source/path.name)
        g.evidence["scope"] = "Stopped form hierarchy/disclosures/invalid drafts/scroll reachability only; no runtime diagnostics or algorithm revalidation; i686 unused"
        g.execute(profile,prefs,steps,verify,lambda gate:gate.main_hwnd() is not None,after)
        g.evidence.update(captures=g.frames,control_and_profile_result="PASS",
            result="PENDING INDEPENDENT VISUAL REVIEW")
        g.save()
        return g.run
    finally:
        g.close()
        lifecycle.remove_startup_key(g.startup_key)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready",action="store_true")
    for name in ("executable","x86-executable","runtime"):
        parser.add_argument("--"+name,type=Path,required=True)
    args = parser.parse_args()
    assert args.ready,"Await frozen READY/exclusive native slot"
    workflow(args)


if __name__ == "__main__":
    main()
