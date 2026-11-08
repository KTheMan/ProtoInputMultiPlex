"""Actual Ouroboros appearance menus/apply/idle/profile and fixed unavailable gate.

Prepare/source-check freely; launch only after root READY/exclusive slot.
"""
import argparse
import copy
import importlib.util
from pathlib import Path
import time
from PIL import Image

def module(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value

maintenance = module("layout_maintenance", "test-profile-maintenance.py")
wheel = module("layout_wheel_ui", "test-horizontal-wheel-ui.py")
ui = maintenance.ui

class LayoutUi(maintenance.ProfileGate):
    def fixtures(self):
        self.args.assigned = False
        try:
            wheel.WheelUi.fixtures(self)
        except Exception as error:
            self.evidence["failure"] = repr(error)
            self.close()
            raise

def live(args):
    gate = LayoutUi(args, "cursor-layout-ui-tail" if args.tail else "cursor-layout-ui", offline=False, count=2)
    gate.fixtures()
    profile = {"version": 1, "concurrent": True,
        "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": gate.pids},
        "seats": [{"id": f"layout-own-{i}", "name": "Owned layout 東京🙂 " + ("界🙂"*40 if i == 0 else "x86"),
            "mouse": f"virtual://mouse/{i+1}", "keyboard": f"virtual://keyboard/{i+1}",
            "desktop_start": gate.points[i]} for i in range(2)]}
    if args.tail:
        profile["seats"][0]["cursor"] = {"shape":"ring", "size":256, "label":True, "badge":"東京🙂",
            "label_position":"above", "label_font_size":48, "badge_shape":"circle", "badge_position":"bottom_right"}
    step = ui.step
    steps = [step("lab", "Input lab"), step("start", "Start routing"),
        step("focus-a", "1 Hold click"), step("focus-a-up", "1 Release click"),
        step("focus-b", "2 Hold click"), step("focus-b-up", "2 Release click"),
        step("configure", "Configure user", {"selected_user": 0}), step("theme", "Theme"),
        step("ring", "Ring cursor", {"size": 256, "label": "true", "badge": "東京🙂"}),
        step("label-menu", "Label position selector"), step("label-above", "Label Above"), step("overlay", "Overlay"),
        step("badge-menu", "Badge shape selector"), step("badge-circle", "Badge Circle"),
        step("position-menu", "Badge position selector"), step("badge-bottom", "Badge Bottom right"),
        step("idle-enable", "Hide cursor after inactivity"),
        step("idle-guard", "Overlay"),
        step("apply", "Apply cursor settings", {"label_font_size": 48, "idle_hide_ms": 600}),
        step("overlay-image", "Overlay"), step("overlay-hold", "Overlay"),
        step("lab-idle", "Input lab"), step("wake", "1 Right"),
        step("stop", "Stop routing"), step("profiles", "Profiles"),
        step("save", "Save profile", {"profile_path": gate.run/"saved.json"}),
        step("reload", "Load profile", {"profile_path": gate.run/"saved.json"}),
        step("roundtrip", "Save profile", {"profile_path": gate.run/"roundtrip.json"})]
    if args.tail:
        keep = {"lab","start","focus-a","focus-a-up","focus-b","focus-b-up","configure","overlay",
                "idle-enable","idle-guard","apply","overlay-image","overlay-hold","lab-idle","wake",
                "stop","profiles","save","reload","roundtrip"}
        steps = [s for s in steps if s["id"] in keep]
    overlays = []
    image_wall = None
    applied = None
    def verify(name, sample):
        nonlocal overlays, image_wall, applied
        state = sample["state"]
        if name == "start":
            if len(state) != 2: return False
            overlays = [u["overlay"]["hwnd"] for u in state]
        cursor = gate.snapshot()["profile"]["seats"][0]["cursor"]
        for event, field, value in (("label-above", "label_position", "above"),
                                    ("badge-circle", "badge_shape", "circle"),
                                    ("badge-bottom", "badge_position", "bottom_right")):
            if name == event: assert cursor[field] == value
        if name == "idle-enable": assert cursor["idle_hide_ms"] == 5000
        if name == "apply":
            assert all(cursor[k] == v for k, v in {"size":256, "label_font_size":48,
                "label_position":"above", "badge_shape":"circle", "badge_position":"bottom_right", "idle_hide_ms":600}.items())
            if not state or state[0]["overlay"]["shape"] != "ring": return False
            o = state[0]["overlay"]
            assert min(o["canvas_size"]) >= 256 and max(o["canvas_size"]) <= 1024
            assert o["badge"] == "東京🙂" and o["label_truncated"] and len(o["rendered_label"]) <= 64
            applied = copy.deepcopy(cursor)
        if name == "overlay-image":
            image_wall = image_wall or time.time()
            path = gate.run/"ui.png"
            if not path.exists() or path.stat().st_mtime < image_wall: return False
            with Image.open(path) as image:
                image.load()
                image.save(gate.run/"cursor-layout-controls.png")
            sample["image_requires_independent_review"] = str(gate.run/"cursor-layout-controls.png")
        if name == "lab-idle":
            if not state[0]["overlay"]["idle_hidden"]: return False
            assert not state[0]["overlay"]["visible"]
            assert state[1]["overlay"]["visible"], "default-off idle hid unrelated user"
        if name == "wake":
            if not state[0]["overlay"]["visible"] or state[0]["overlay"]["idle_hidden"]: return False
        if name == "stop":
            if any(p["held_keys"] or p["buttons"] for p in sample["probes"]): return False
            if any(ui.user32.IsWindow(h) for h in overlays): return False
        if name in ("save", "roundtrip"):
            path = gate.run/("saved.json" if name == "save" else "roundtrip.json")
            saved = ui.read_json(path)
            assert saved["seats"][0]["cursor"] == applied
            if name == "roundtrip": assert saved == ui.read_json(gate.run/"saved.json")
        return True
    try:
        gate.execute(profile, steps, verify)
        return gate.run
    finally: gate.close()

def fixed(args):
    gate = LayoutUi(args, "cursor-layout-fixed-tail" if args.tail else "cursor-layout-fixed", offline=True, count=0)
    profile = {"version":1, "concurrent":False, "seats":[{"id":"layout-fixed", "name":"Owned fixed layout",
        "mouse":"virtual://mouse/71", "keyboard":"virtual://keyboard/71", "target_pid":991001}]}
    invalid = copy.deepcopy(profile)
    invalid["seats"][0].update(name="Rejected extended layout", cursor={"size":256, "idle_hide_ms":500})
    ui.put(gate.run/"invalid.json", invalid)
    step = ui.step
    steps = [step("configure", "Configure user", {"selected_user":0}), step("theme", "Theme")]
    for i, label in enumerate(("Cursor size", "Label position selector", "Label font size")):
        steps.append(step(f"disabled-{i}", label, disabled=True))
    steps.append(step("overlay", "Overlay"))
    for i, label in enumerate(("Badge shape selector", "Badge position selector", "Hide cursor after inactivity"), 3):
        steps.append(step(f"disabled-{i}", label, disabled=True))
    steps += [step("profiles", "Profiles"), step("invalid-load", "Load profile", {"profile_path":gate.run/"invalid.json"}),
              step("save", "Save profile", {"profile_path":gate.run/"retained.json"})]
    if args.tail:
        steps = [s for s in steps if s["id"] in ("configure", "profiles", "invalid-load", "save")]
    expected = None
    def verify(name, sample):
        nonlocal expected
        assert not (gate.run/"ui.state.json").exists(), "Fixed unavailable gate started a session"
        snapshot = gate.snapshot()
        if name == "configure": expected = snapshot["profile"]
        if name == "invalid-load":
            assert snapshot["profile"] == expected
            message = snapshot["status"].lower()
            assert all(word in message for word in ("appearance", "require", "desktop")), message
        if name == "save": assert ui.read_json(gate.run/"retained.json") == expected
        return True
    try:
        gate.execute(profile, steps, verify)
        return gate.run
    finally: gate.close()

def main():
    parser = argparse.ArgumentParser()
    for name in ("executable", "x86-executable", "runtime"): parser.add_argument("--"+name, type=Path, required=True)
    parser.add_argument("--ready", action="store_true")
    parser.add_argument("--phase", choices=("live", "fixed"), default="live")
    parser.add_argument("--tail", action="store_true", help="Only unfinished idle/apply/image/profile tail after accepted menu prefix")
    args = parser.parse_args()
    assert args.ready, "Wait for root READY/exclusive slot"
    (live if args.phase == "live" else fixed)(args)

if __name__ == "__main__": main()
