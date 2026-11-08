"""Finite stopped Settings progressive-disclosure gate; frozen READY required."""
import argparse
import copy
import hashlib
import importlib.util
import io
from pathlib import Path
import shutil
import time


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def workflow(args, label, baseline_profile=None):
    lifecycle = module("hub_lifecycle", "test-app-lifecycle-ui.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    from PIL import Image, ImageChops

    class Gate(Base):
        def __init__(self):
            super().__init__(args, "settings-hub-"+label, offline=True, count=0)
            self.frames = {}
            self.started = {}
            self.deduplicated = set()

        def frame(self, name, size, *, initial=False, identical_to=None):
            if name in self.frames or name in self.deduplicated:
                return True
            hwnd = self.main_hwnd()
            if not hwnd:
                return False
            if name not in self.started:
                if not initial:
                    geometry.resize_owned_client(hwnd, size)
                self.started[name] = time.time()
                return False
            current = geometry.geometry(hwnd)
            if any(abs(a-b)>2 for a, b in zip(current["client_logical"], size)):
                return False
            if identical_to:
                source = self.run/"ui.png"
                try:
                    before = source.stat()
                    data = source.read_bytes()
                    after = source.stat()
                    if before.st_mtime <= self.started[name] or (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
                        return False
                    with Image.open(io.BytesIO(data)) as image, Image.open(self.run/(identical_to+".png")) as previous:
                        image.load()
                        if list(image.size) != current["client_pixels"]:
                            return False
                        if ImageChops.difference(image.convert("RGB"), previous.convert("RGB")).getbbox() is None:
                            self.deduplicated.add(name)
                            self.evidence["minimum_lower_reveal"] = "Next actual Output Rules target left the owned Tools minimum image RGB-identical; redundant frame not saved"
                            return True
                except (OSError, ValueError):
                    return False
            shot = geometry.copy_owned_product_frame(self.run/"ui.png", self.run/(name+".png"),
                                                     hwnd, self.started[name])
            if not shot:
                return False
            self.frames[name] = shot
            return True

    g = Gate()
    profile = {"version": 1, "concurrent": True,
               "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": [987301, 987302]},
               "seats": [{"id": f"hub-owned-{i}", "name": "Owned Alpha" if i == 0 else "Owned Beta",
                          "mouse": f"virtual://mouse/{i+1}", "keyboard": f"virtual://keyboard/{i+1}",
                          "cursor": {"color": [70,190,160] if i == 0 else [230,140,60]}}
                         for i in range(2)]}
    prefs = {"version": 1, "appearance_mode": "dark", "zoom_percent": 100,
             "launch_minimized": False, "minimize_to_tray": False, "close_to_tray": False,
             "launch_at_login": False, "shortcuts": []}
    step = ui.step
    steps = [step("settings", "Settings")]
    if label == "candidate":
        steps += [step("application", "Application preferences"), step("home-app", "Back to Home"), step("settings-app", "Settings"),
                  step("users-category", "Settings category Users & devices"), step("users", "Users & devices"),
                  step("home-users", "Back to Home"), step("settings-users", "Settings"),
                  step("automation-category", "Settings category Automation"), step("automation", "Macros & replication"),
                  step("home-automation", "Back to Home"), step("settings-automation", "Settings"),
                  step("tools-category", "Settings category Tools"), step("tools-guard", "Settings category Tools"),
                  step("tools", "Output Rules"), step("home-tools", "Back to Home"), step("settings-tools", "Settings"),
                  step("agent-category", "Settings category Agent access"), step("agent", "Agent Access"),
                  step("home-agent", "Back to Home"), step("settings-remembered", "Settings")]
        assert len(steps) == 21
    expected = None
    destinations = {"application": "Application preferences", "users": "Users & devices",
                    "automation": "Macros & replication", "tools": "Output Rules", "agent": "Agent Access"}

    def verify(name, sample):
        nonlocal expected
        current = g.snapshot()["profile"]
        if expected is None:
            expected = copy.deepcopy(current)
            if baseline_profile is not None:
                assert current == baseline_profile, "Candidate changed exact normalized profile"
        assert current == expected, "Settings navigation/category changed profile"
        assert ui.read_json(g.preferences_path) == prefs, "Settings navigation changed preferences"
        assert not (g.run/"ui.state.json").exists(), "Stopped Settings gate created worker"
        sample["profile_preferences_unchanged"] = True
        if name in destinations:
            sample["representative_destination_receipt"] = destinations[name]
        if name == "settings":
            if not g.frame("settings-baseline" if label == "baseline" else "settings-application",
                           [540,480], initial=label == "candidate"):
                return False
        if name == "automation-category" and not g.frame("automation-minimum", [320,360]):
            return False
        if name == "tools-category":
            for frame, size in [("tools-normal", [540,480]), ("tools-minimum-upper", [320,360])]:
                if not g.frame(frame, size):
                    return False
        if name == "tools-guard" and not g.frame("tools-minimum-lower", [320,360], identical_to="tools-minimum-upper"):
            return False
        if name == "agent":
            agent = ui.read_json(g.run/"ui.agent.json")
            assert not agent["policy_enabled"] and not agent["session_control"]
            assert not agent["capture"] and not agent["app_preferences"] and not agent["clients"]
            assert not agent["pending"]
            sample["agent_permissions_remain_off"] = agent
        if name == "settings-remembered" and not g.frame("settings-remembered-agent", [540,480], initial=True):
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
        shutil.copy2(Path(__file__), provenance/Path(__file__).name)
        g.evidence["scope"] = "Stopped Settings categories/navigation/representative owned pixels only; no worker/MCP/probe/DLL/permissions mutation/feature algorithm/full UX claim; i686 unused"
        g.execute(profile, prefs, steps, verify, lambda gate: gate.main_hwnd() is not None, after)
        g.evidence.update(result="PENDING INDEPENDENT VISUAL REVIEW", control_profile_result="PASS", captures=g.frames)
        g.save()
        return g.run, expected
    finally:
        g.close()
        lifecycle.remove_startup_key(g.startup_key)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready", action="store_true")
    for name in ("baseline-executable", "executable", "x86-executable", "runtime"):
        parser.add_argument("--"+name, type=Path, required=True)
    args = parser.parse_args()
    assert args.ready, "Await frozen image/source and exclusive slot"
    assert hashlib.sha256(args.baseline_executable.read_bytes()).hexdigest() == "76cc3cab281563ffa2bae93ef8a0b3a0732725ff289c8a2e4454eed692ed741f"
    baseline_args = copy.copy(args)
    baseline_args.executable = args.baseline_executable
    baseline, profile = workflow(baseline_args, "baseline")
    candidate, _ = workflow(args, "candidate", profile)
    print(f"PENDING independent Settings visual review: {baseline} ; {candidate}", flush=True)


if __name__ == "__main__":
    main()
