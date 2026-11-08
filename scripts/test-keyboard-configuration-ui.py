"""Finite local-egui keyboard + exact owned-HWND native UIA gate. ROOT READY only."""
import argparse
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import time


def module(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def freeze(args):
    assert args.ready, "Await frozen source/producer/ROOT READY"
    assert digest(args.producer) == args.producer_sha256.lower()
    producer = json.loads(args.producer.read_text(encoding="utf-8"))
    assert producer["result"] == "PASS"
    assert len(producer["checks"]) == 5 and all(check["exit_code"] == 0 for check in producer["checks"])
    assert producer["source_before_sha256"] == producer["source_after_sha256"]
    repo = Path(__file__).resolve().parent.parent
    assert all(digest(repo / name) == value for name, value in producer["source_after_sha256"].items())
    assert digest(args.executable) == producer["build_sha256"]["x64"]
    assert digest(args.x86_executable) == producer["build_sha256"]["i686"]
    assert all(digest(args.runtime / name) == value for name, value in producer["runtime_sha256"].items())


def workflow(args):
    lifecycle = module("keyboard_lifecycle", "test-app-lifecycle-ui.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()

    class Gate(Base):
        @staticmethod
        def assert_event_contract(steps, events):
            assert len(events) <= len(steps)
            pairs = 0
            for spec, event in zip(steps, events):
                assert event["button"] == spec["button"]
                if spec.get("key"):
                    assert event["key"] == spec["key"] and event["focused"] is True
                    assert isinstance(event["key_pairs"], int) and 1 <= event["key_pairs"] <= 24
                    assert event["clicked"] is (spec["key"] in ("Enter", "Space"))
                    if spec["key"] not in ("Tab", "ShiftTab"):
                        assert event["key_pairs"] == 1
                    pairs += event["key_pairs"]
                else:
                    Base.assert_event_contract([spec], [event])
            assert pairs <= 48, "App-local key budget exceeded"

        def native_uia(self, name, value=None):
            output = self.run / (name + ".json")
            helper = self.run / "runner-source" / "keyboard-ux-uia-helper.ps1"
            command = ["powershell.exe", "-NoProfile", "-NonInteractive", "-File", str(helper),
                       "-OwnedHwnd", str(self.main_hwnd()), "-OwnedPid", str(self.gui.pid),
                       "-OutputPath", str(output)]
            if value is not None:
                command += ["-SliderName", "Horizontal motion speed for Keyboard B", "-SetValue", str(value)]
            process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                       creationflags=subprocess.CREATE_NO_WINDOW)
            self.owned.append(process)
            stdout, stderr = process.communicate(timeout=15)
            (self.run / (name + ".stderr.txt")).write_bytes(stderr)
            assert process.returncode == 0, (name, process.returncode, stderr.decode(errors="replace"))
            result = json.loads(output.read_text(encoding="utf-8-sig"))
            self.evidence.setdefault("native_uia", {})[name] = result
            self.save()
            return result

    g = Gate(args, "keyboard-configuration", offline=False, count=1)
    layers = set()
    baseline = None
    accepted_speed = 1.0
    uia_written = False
    try:
        target = subprocess.Popen([str(g.images[0]), "probe", "--automation", "--report", str(g.run / "probe-0.json")],
                                  creationflags=subprocess.CREATE_NO_WINDOW)
        g.owned.append(target)
        ui.eventually(lambda: g.probes(), lambda reports: reports[0]["frames"] > 2)
        profile = {"version": 1, "concurrent": True,
                   "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": [target.pid]},
                   "seats": [{"id": "keyboard-" + suffix, "name": "Keyboard " + suffix.upper(),
                              "mouse": f"virtual://mouse/{index+1}", "keyboard": f"virtual://keyboard/{index+1}",
                              "desktop_start": [130+index*90, 160], "cursor": {"speed_x": 1.0, "lock_window": False}}
                             for index, suffix in enumerate(("a", "b"))]}
        preferences = {"version": 1, "appearance_mode": "dark", "zoom_percent": 100,
                       "launch_minimized": False, "minimize_to_tray": False, "close_to_tray": False,
                       "launch_at_login": False, "shortcuts": []}

        def key(name, button, value):
            step = ui.step(name, button)
            step["key"] = value
            return step

        steps = [ui.step("anchor", "Select user 1"),
                 key("tab-b", "Select user 2", "Tab"), key("select-b", "Select user 2", "Enter"),
                 key("tab-start", "Start routing", "Tab"), key("start", "Start routing", "Enter"),
                 key("tab-configure", "Configure user", "Tab"), key("configure", "Configure user", "Enter"),
                 ui.step("disabled-remove", "Remove this user", disabled=True),
                 key("tab-motion", "Motion", "Tab"), key("motion", "Motion", "Enter"),
                 key("tab-slider", "Motion horizontal speed", "Tab"),
                 key("right", "Motion horizontal speed", "ArrowRight"),
                 key("minimum", "Motion horizontal speed", "Home"),
                 key("maximum", "Motion horizontal speed", "End"),
                 key("page-down-uia", "Motion horizontal speed", "PageDown"),
                 key("tab-apply", "Apply cursor settings", "Tab"),
                 key("apply", "Apply cursor settings", "Enter"),
                 ui.step("home", "Back to Home"), ui.step("stop", "Stop routing"),
                 ui.step("settings", "Settings"), ui.step("users", "Settings category Users & devices"),
                 ui.step("profiles", "Profiles & backend"), ui.step("save", "Save profile"),
                 ui.step("reload", "Load profile")]
        assert len(steps) == 24 and sum(not s.get("key") and not s.get("expect_disabled") for s in steps) == 8
        values = {"right": 1.1, "minimum": .1, "maximum": 8.0, "page-down-uia": 7.0}
        source = g.run / "runner-source"
        source.mkdir()
        shutil.copy2(Path(__file__), source / Path(__file__).name)
        shutil.copy2(Path(__file__).resolve().parent.parent / "runtime/keyboard-ux-uia-helper.ps1", source / "keyboard-ux-uia-helper.ps1")
        g.evidence.update(producer=str(args.producer), producer_sha256=digest(args.producer),
                          limits="Generic Apply InputSent acknowledgement; no independent worker speed readback or routed input.")

        def verify(name, sample):
            nonlocal baseline, accepted_speed, uia_written
            actual = sample["profile_state"]["profile"]
            if baseline is None:
                baseline = copy.deepcopy(actual)
            unchanged = copy.deepcopy(actual)
            actual_speed = unchanged["seats"][1]["cursor"]["speed_x"]
            unchanged["seats"][1]["cursor"]["speed_x"] = baseline["seats"][1]["cursor"]["speed_x"]
            assert unchanged == baseline, "Mutation outside selected B horizontal speed"
            assert ui.read_json(g.preferences_path) == preferences
            expected = 1.3 if uia_written else values.get(name, accepted_speed)
            if abs(actual_speed - expected) > 1e-5:
                previous = [accepted_speed] + ([7.0] if name == "page-down-uia" and uia_written else [])
                assert any(abs(actual_speed - value) < 1e-5 for value in previous), (name, actual_speed, expected)
                return False
            if name == "select-b":
                result = g.once("home-native-tree", lambda: g.native_uia("home-native-tree"))
                names = [node["name"] for node in result["nodes"]]
                assert any(n.startswith("Select user 1, Keyboard A,") and n.endswith(", not selected") for n in names)
                assert any(n.startswith("Select user 2, Keyboard B,") and "virtual pointer" in n and "keyboard paired" in n and n.endswith(", selected") for n in names)
                shot = g.screenshot("home-keyboard-selection")
                if not shot:
                    return False
                sample["owned_original"] = shot
            if name == "start":
                layers.update(state["overlay"]["hwnd"] for state in sample["state"] if state.get("overlay"))
                assert len(layers) == 2 and all(h and ui.user32.IsWindow(h) for h in layers)
            if name == "page-down-uia":
                if not uia_written:
                    tree = g.native_uia("motion-native-tree")
                    nodes = [node for node in tree["nodes"] if node["name"] == "Horizontal motion speed for Keyboard B"]
                    assert len(nodes) == 1
                    node = nodes[0]
                    assert node["role"] == "ControlType.Slider" and node["enabled"] and not node["offscreen"]
                    value = node["range"]
                    assert not value["read_only"] and abs(value["value"] - 7.0) < 1e-5
                    assert abs(value["minimum"] - .1) < 1e-5 and abs(value["maximum"] - 8.0) < 1e-5
                    assert abs(value["small_change"] - .1) < 1e-5
                    g.native_uia("motion-native-setvalue", 1.3)
                    uia_written = True
                    return False
                shot = g.screenshot("motion-native-range")
                if not shot:
                    return False
                sample["owned_original"] = shot
            if name == "tab-apply":
                g.evidence["pre_apply_status"] = sample["profile_state"]["status"]
                assert sample["profile_state"]["status"] != "Emulated input sent through ProtoInput."
            if name == "apply":
                if sample["profile_state"]["status"] != "Emulated input sent through ProtoInput.":
                    return False
                g.evidence["apply_generic_ack"] = sample["profile_state"]["status"]
            if name == "stop" and any(ui.user32.IsWindow(h) for h in layers):
                return False
            if name in ("save", "reload"):
                saved = ui.read_json(g.run / "profile.json")
                assert saved == actual and abs(saved["seats"][1]["cursor"]["speed_x"] - 1.3) < 1e-5
                g.evidence["saved_profile"] = saved
                if name == "reload":
                    shot = g.screenshot("saved-and-reloaded")
                    if not shot:
                        return False
                    sample["owned_original"] = shot
            accepted_speed = expected
            return True

        def after(gate):
            gate.expected_exit = True
            assert geometry.user32.PostMessageW(gate.main_hwnd(), 0x10, 0, 0)
            gate.gui.wait(timeout=10)
            assert gate.gui.returncode == 0 and not hotkeys.lifecycle_window(gate.gui.pid)
            probe_hwnd = geometry.owned_window(target.pid)
            assert probe_hwnd and geometry.user32.PostMessageW(probe_hwnd, 0x10, 0, 0)
            target.wait(timeout=10)
            assert target.returncode == 0
            assert g.probes()[0]["key_events"] == []
            assert not any(ui.user32.IsWindow(h) for h in layers)
            return True

        g.execute(profile, preferences, steps, verify, after_controls=after)
        g.evidence["result"] = "PENDING INDEPENDENT DATA/NATIVE UIA/PIXEL REVIEW"
        g.evidence["key_pairs"] = sum(event.get("key_pairs", 0) for event in g.evidence["clicked_events"])
    finally:
        g.close()
        lifecycle.remove_startup_key(g.startup_key)
        g.evidence["known_overlays_closed"] = all(not ui.user32.IsWindow(h) for h in layers)
        try:
            g.evidence["final_progress"] = ui.read_json(g.run / "ui.progress.json")
        except (OSError, ValueError):
            pass
        g.save()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready", action="store_true")
    for name in ("executable", "x86-executable", "runtime", "producer"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--producer-sha256", required=True)
    args = parser.parse_args()
    freeze(args)
    workflow(args)


if __name__ == "__main__":
    main()
