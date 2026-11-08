"""Actual Preserve/Apply/Input Lab/profile workflow and bounded assigned smoke."""
import argparse
from ctypes import wintypes
import importlib.util
from pathlib import Path
import subprocess
import time
from PIL import Image

_spec = importlib.util.spec_from_file_location("wheel_ui_helpers", Path(__file__).with_name("test-replication-graph-ui.py"))
_ui = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_ui)
_spec = importlib.util.spec_from_file_location("wheel_native_helpers", Path(__file__).with_name("test-horizontal-wheel-acceptance.py"))
_wheel = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_wheel)
step, read_json, eventually = _ui.step, _ui.read_json, _ui.eventually


class WheelUi(_ui.UiGate):
    def fixtures(self):
        for i in range(2):
            self.owned.append(subprocess.Popen([str(self.images[i]), "probe", "--automation", "--report",
                str(self.run / f"probe-{i}.json")], creationflags=subprocess.CREATE_NO_WINDOW))
        self.pids = [p.pid for p in self.owned]
        client = _ui.Client(self.images[0], self.args.runtime.resolve(), self.run / "discovery.stderr.log")
        try:
            tools = client.rpc("tools/list", {})["tools"]
            assert {t["name"] for t in tools} == _wheel.WheelGate.tool_names
            assert all(t["inputSchema"]["type"] == "object" for t in tools)
            self.evidence["tools"] = tools
            inventory = eventually(lambda: client.tool("routing_windows"), lambda v:
                all(any(w["pid"] == p and w["title"].startswith(_wheel._base.PREFIX) for w in v["windows"])
                    for p in self.pids), 60)
            for i, pid in enumerate(self.pids):
                window = next(w for w in inventory["windows"] if w["pid"] == pid
                    and w["title"].startswith(_wheel._base.PREFIX))
                x, y = (-250, -250) if self.args.assigned and i == 0 else (30+i*460, 40)
                assert _ui.user32.SetWindowPos(window["hwnd"], wintypes.HWND(-1), x, y, 420, 360, 0x10)
            inventory = client.tool("routing_windows")
            self.windows = [next(w for w in inventory["windows"] if w["pid"] == p
                and w["title"].startswith(_wheel._base.PREFIX)) for p in self.pids]
            self.points = [[w["client"]["left"]+120, w["client"]["top"]+100] for w in self.windows]
            if self.args.assigned:
                # Assigned Input Lab exposes relative arrows; absolute Move to point
                # is an automatic-desktop control. Start at the actual root center,
                # then use its real Left control to establish the negative anchor.
                self.points = [[(w["client"]["left"]+w["client"]["right"])//2,
                                (w["client"]["top"]+w["client"]["bottom"])//2] for w in self.windows]
                self.points[0][0] -= 40
        finally:
            client.close()

    def run_gate(self):
        self.fixtures()
        profile = {"version": 1, "concurrent": True,
            "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": self.pids},
            "seats": [{"id": f"wheel-owned-{i}", "name": f"Owned wheel UI {i+1}",
                "mouse": f"virtual://mouse/{i+1}", "keyboard": f"virtual://keyboard/{i+1}",
                "desktop_start": self.points[i], "cursor": {"lock_window": self.args.assigned}}
                for i in range(2)]}
        if self.args.assigned:
            profile["desktop"] = None
            for i, seat in enumerate(profile["seats"]):
                seat["target_pid"] = self.pids[i]
        steps = [step("lab", "Input lab"), step("start", "Start routing")]
        if not self.args.assigned:
            for i in range(2):
                steps += [step(f"select-{i}", f"{i+1} Hold click"), step(f"drop-{i}", f"{i+1} Release click")]
        else:
            steps += [step("negative-point", "1 Left")]
        steps += [step("other-hold", "Hold key", {"selected_user": 1, "key_name": "f6"}),
            step("configure", "Configure user", {"selected_user": 0}), step("wheel-tab", "Wheel"),
            step("preserve", "Preserve horizontal wheel"),
            step("apply", "Apply cursor settings", {"horizontal_wheel_scale": 2, "invert_horizontal_wheel": "true"}),
            step("apply-image", "Apply cursor settings"), step("wheel-lab", "Input lab"),
            step("horizontal-input", "1 Horizontal wheel"), step("vertical-input", "1 Wheel"),
            step("release-other", "Release key", {"selected_user": 1, "key_name": "f6"}), step("stop", "Stop routing")]
        if not self.args.assigned:
            steps += [step("profiles", "Profiles"), step("save", "Save profile", {"profile_path": self.run / "saved.json"}),
                step("reload", "Load profile", {"profile_path": self.run / "saved.json"}),
                step("roundtrip", "Save profile", {"profile_path": self.run / "roundtrip.json"})]
        before, overlays, image_wall = {}, [], None
        def verify(name, sample):
            nonlocal overlays, image_wall
            probes, state = sample["probes"], sample["state"]
            if name == "start":
                if len(state) != 2:
                    return False
                overlays = [u["overlay"]["hwnd"] for u in state]
            if name.startswith("select-"):
                if not probes[int(name[-1])]["buttons"] & 1:
                    return False
            if name.startswith("drop-") and probes[int(name[-1])]["buttons"]:
                return False
            if name == "negative-point":
                assert self.points[0][0] < 0 and self.points[0][1] < 0
                if probes[0]["cursor"] != self.points[0]:
                    return False
            if name == "other-hold" and 117 not in probes[1]["held_keys"]:
                return False
            if name == "apply":
                if len(state) != 2 or 117 not in probes[1]["held_keys"]:
                    return False
                image_wall = image_wall or time.time()
                png = self.run / "ui.png"
                if not png.exists() or png.stat().st_mtime < image_wall:
                    return False
                with Image.open(png) as image:
                    image.load()
                    image.save(self.run / "preserve-controls.png")
                sample["owned_settings_screenshot"] = str(self.run / "preserve-controls.png")
            if name == "wheel-lab":
                before["state"] = _wheel.WheelGate.protected({"users": state})
                before["probes"] = probes
            if name == "horizontal-input":
                if probes[0]["horizontal_wheel_events"] != before["probes"][0]["horizontal_wheel_events"]+1:
                    return False
                event = [e for e in probes[0]["native_events"] if e["kind"] == "horizontal_wheel"][-1]
                assert event["delta"] == -240 and event["wheel_screen"] == self.points[0], event
                assert event["wheel_modifiers"] == 0, event
                assert probes[0]["wheel_events"] == before["probes"][0]["wheel_events"]
                assert probes[1]["horizontal_wheel_events"] == before["probes"][1]["horizontal_wheel_events"]
                assert probes[0]["mouse_event_count"] == before["probes"][0]["mouse_event_count"]
                assert _wheel.WheelGate.protected({"users": state}) == before["state"]
                assert 117 in probes[1]["held_keys"]
            if name == "vertical-input":
                if probes[0]["wheel_events"] != before["probes"][0]["wheel_events"]+1:
                    return False
                assert probes[0]["wheel_delta"] == before["probes"][0]["wheel_delta"]+120
                assert probes[1]["wheel_events"] == before["probes"][1]["wheel_events"]
            if name == "release-other" and 117 in probes[1]["held_keys"]:
                return False
            if name == "stop":
                if any(p["held_keys"] or p["buttons"] for p in probes) or any(_ui.user32.IsWindow(h) for h in overlays):
                    return False
            if name in ("save", "roundtrip"):
                saved = read_json(self.run / ("saved.json" if name == "save" else "roundtrip.json"))
                cursor = saved["seats"][0]["cursor"]
                assert cursor["horizontal_wheel"] == "preserve" and cursor["horizontal_wheel_scale"] == 2
                assert cursor["invert_horizontal_wheel"] is True
                assert saved["seats"][1]["cursor"]["horizontal_wheel"] == "ignore"
                if name == "roundtrip":
                    assert saved == read_json(self.run / "saved.json")
                sample["saved_profile"] = saved
            return True
        self.execute(profile, steps, verify)


def main():
    parser = argparse.ArgumentParser()
    for name in ("executable", "x86-executable", "runtime"):
        parser.add_argument("--"+name, type=Path, required=True)
    parser.add_argument("--ready", action="store_true")
    parser.add_argument("--assigned", action="store_true")
    args = parser.parse_args()
    assert args.ready, "No native launch before producer READY"
    gate = WheelUi(args, "assigned-horizontal-wheel" if args.assigned else "horizontal-wheel", count=2)
    try:
        gate.run_gate()
    finally:
        for process in reversed(gate.owned):
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=15)
        gate.evidence["owned_processes_closed"] = all(p.poll() is not None for p in gate.owned)
        gate.save()


if __name__ == "__main__":
    main()
