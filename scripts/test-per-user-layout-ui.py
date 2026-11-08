"""Bounded per-user layout gate. Import/AST preparation never launches an app.

Run only after root supplies frozen images and the exclusive native slot.
Control receipts/geometry remain PENDING independent review of original pixels.
"""
import argparse
import copy
import ctypes
import hashlib
import importlib.util
import json
import shutil
import subprocess
import time
from pathlib import Path


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def fixture():
    return {"version": 1, "concurrent": True, "desktop": None,
        "seats": [{"id": "layout-owned-" + str(i),
            "name": ("Owned long per-user 東京🙂 " + "bounded name " * 8) if i == 0 else "Owned Beta",
            "mouse": f"virtual://mouse/{i+1}", "keyboard": f"virtual://keyboard/{i+1}",
            "target_pid": 987001 + i,
            "cursor": {"color": [60, 210, 175] if i == 0 else [245, 150, 70],
                "speed_x": 1.3 + i, "speed_y": 1.6 + i, "shape": "arrow" if i == 0 else "ring",
                "horizontal_wheel": "ignore", "wheel_scale": 1.5 + i,
                "badge": "First" if i == 0 else "Second", "bounds_mode": "legacy"}}
            for i in range(2)]}


def workflow(args):
    lifecycle = module("per_user_lifecycle", "test-app-lifecycle-ui.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()

    class Gate(Base):
        def __init__(self):
            super().__init__(args, "per-user-layout-" + args.phase,
                offline=args.phase == "offline", count=0 if args.phase == "offline" else 1)
            self.frames = {}
            self.capture_started = {}
            self.frame_sizes = {}

        def frame(self, name, size):
            if name in self.frames:
                return self.frames[name]
            hwnd = self.main_hwnd()
            if not hwnd:
                return None
            # Resize our exact owned client only. No pointer/input injection.
            if self.frame_sizes.get(name) != size:
                geometry.resize_owned_client(hwnd, size)
                self.frame_sizes[name] = size
                self.capture_started[name] = time.time()
                return None
            current = geometry.geometry(hwnd)
            if any(abs(a-b) > 2 for a,b in zip(current["client_logical"], size)):
                return None
            shot = geometry.copy_owned_product_frame(self.run/"ui.png", self.run/(name+".png"),
                hwnd, self.capture_started[name])
            if shot:
                shot["geometry"] = current
                self.frames[name] = shot
            return shot

        def pair(self, name):
            # Immutable frames; no model-review wait while the app is open.
            for suffix, size in (("normal", [680, 620]), ("minimum", [320, 360])):
                if not self.frame(name+"-"+suffix, size):
                    return False
            return True

        def initial_frame(self, name, size):
            """Observe the product-chosen initial size; never resize to pass."""
            if name in self.frames:
                return self.frames[name]
            hwnd = self.main_hwnd()
            if not hwnd:
                return None
            self.capture_started.setdefault(name, time.time())
            current = geometry.geometry(hwnd)
            if any(abs(a-b) > 2 for a,b in zip(current["client_logical"], size)):
                return None
            shot = geometry.copy_owned_product_frame(self.run/"ui.png", self.run/(name+".png"),
                hwnd, self.capture_started[name])
            if shot:
                shot["geometry"] = current
                shot["product_chosen_initial_size_not_resized"] = True
                self.frames[name] = shot
            return shot

    g = Gate()
    profile = fixture()
    if args.continuation:
        assert args.phase == "offline" and args.from_run
        prefix_path = args.from_run.resolve()/"evidence.json"
        prefix = json.loads(prefix_path.read_text(encoding="utf-8-sig"))
        accepted = ["configure", "input", "keyboard", "output", "motion", "buttons",
            "wheel", "wheel-choice", "theme", "overlay"]
        assert prefix["result"] == "NOT ACCEPTED" and prefix["owned_processes_closed"]
        assert list(prefix["samples"]) == accepted and len(prefix["clicked_events"]) == 10
        assert set(prefix["build_sha256"].values()) == {
            "4480f1a6814f60b856cbe55766ae25d53e1c3e427d009a32d5ae0babdcd98287",
            "434582973d7e3d04d521e3ef2f20050ada0f5f25ad98160d9a1817e00b1d19c5"}
        profile = copy.deepcopy(prefix["samples"]["overlay"]["profile_state"]["profile"])
        assert len(profile["seats"]) == 2
        assert profile["seats"][0]["cursor"]["horizontal_wheel"] == "preserve"
        assert profile["seats"][0]["cursor"]["idle_hide_ms"] is None
        assert profile["seats"][1]["cursor"]["horizontal_wheel"] == "ignore"
        g.evidence["retained_prefix"] = {"evidence": str(prefix_path),
            "sha256": hashlib.sha256(prefix_path.read_bytes()).hexdigest(),
            "original_result": prefix["result"], "accepted_controls": accepted,
            "scope": "Fresh explicit continuation fixture seeded from accepted state; not profile persistence or replay of eight accepted tabs"}
    if args.reveal_only:
        assert args.phase == "offline" and args.from_run
        prefix_path = args.from_run.resolve()/"evidence.json"
        prefix = json.loads(prefix_path.read_text(encoding="utf-8-sig"))
        assert prefix["result"] == "PENDING INDEPENDENT VISUAL REVIEW"
        assert prefix["control_and_profile_result"] == "PASS" and prefix["owned_processes_closed"]
        assert len(prefix["samples"]) == 17
        assert set(prefix["build_sha256"].values()) == {
            "26498d1d8d73fbb549f99b5a490ea02cad51095e3b7304b15ed9269f0e26ca27",
            "b29d0865c6dd770dc17bb0cc7d658bd7845048804eb96ad26c505647d2edc316"}
        profile = copy.deepcopy(prefix["samples"]["seat-1-wheel"]["profile_state"]["profile"])
        cursor = profile["seats"][0]["cursor"]
        assert cursor["horizontal_wheel"] == "preserve" and cursor["visible"] is True
        assert cursor["bounds_mode"] == "legacy" and cursor["idle_hide_ms"] == 5000
        g.evidence["retained_prefix"] = {"evidence": str(prefix_path),
            "sha256": hashlib.sha256(prefix_path.read_bytes()).hexdigest(),
            "original_result": prefix["result"], "accepted_observations": 17,
            "scope": "Only missing minimum full-control reveal; fresh same-value fixture, not persistence"}
    prefs = {"version": 1, "appearance_mode": "dark", "zoom_percent": 100,
        "launch_minimized": False, "minimize_to_tray": False,
        "close_to_tray": False, "launch_at_login": False,
        "shortcuts": [{"action": "stop", "key": "f11", "modifiers":
            {"control": True, "alt": True, "shift": True, "windows": False}}]}
    step = ui.step
    if args.phase == "offline":
        steps = [step("configure", "Configure user"), step("input", "Input"),
            step("keyboard", "Keyboard"), step("output", "Output"), step("motion", "Motion"),
            step("buttons", "Buttons"), step("wheel", "Wheel"),
            step("wheel-choice", "Preserve horizontal wheel"), step("theme", "Theme"),
            step("overlay", "Overlay"), step("overlay-choice", "Hide cursor after inactivity"),
            step("lock", "Lock"), step("error-stage", "Lock"),
            step("invalid-footer", "Apply cursor settings", {"bounds_mode": "monitor",
                "monitor_id": "OWN-UNAVAILABLE-DISPLAY"}, disabled=True),
            step("restore-bounds", "Legacy bounds"), step("home", "Back to Home"),
            step("seat-2", "Select user 2"), step("configure-2", "Configure user"),
            step("seat-2-overlay", "Overlay"), step("home-2", "Back to Home"),
            step("seat-1", "Select user 1"), step("configure-1", "Configure user"),
            step("seat-1-wheel", "Wheel")]
        if args.continuation:
            # Only changed-header representative captures and genuinely missing
            # controls. No Keyboard/Output/Motion/Buttons/Theme replay.
            steps = [s for s in steps if s["id"] not in
                {"keyboard", "output", "motion", "buttons", "wheel-choice", "theme"}]
        if args.reveal_only:
            steps = [step("configure", "Configure user"), step("wheel", "Wheel"),
                step("reveal-wheel", "Preserve horizontal wheel"), step("overlay", "Overlay"),
                step("reveal-overlay", "Show cursor"), step("lock", "Lock"),
                step("reveal-lock", "Legacy bounds"), step("error-stage", "Lock"),
                step("invalid-footer", "Apply cursor settings", {"bounds_mode": "monitor",
                    "monitor_id": "OWN-UNAVAILABLE-DISPLAY"}, disabled=True),
                step("restore-bounds", "Legacy bounds")]
    else:
        # One own x64 target only; no historical delivery/tool-discovery matrix.
        probe = subprocess.Popen([str(g.images[0]), "probe", "--automation", "--report",
            str(g.run/"probe-0.json")], creationflags=subprocess.CREATE_NO_WINDOW)
        g.owned.append(probe)
        g.pids = [probe.pid]
        profile["seats"] = profile["seats"][:1]
        profile["seats"][0].pop("target_pid")
        profile["desktop"] = {"mode": "concurrent", "multi_keyboard": True,
            "allowed_pids": g.pids}
        steps = [step("start", "Start routing"), step("configure", "Configure user"),
            step("theme", "Theme"), step("choice", "Crosshair cursor"),
            step("apply", "Apply cursor settings"), step("input", "Input"),
            step("remove-disabled", "Remove this user", disabled=True),
            step("keyboard", "Keyboard"), step("home", "Back to Home"),
            step("stop", "Stop routing")]

    baseline = None
    expected = None
    overlay_hwnds = []
    transitions = {}
    def verify(name, sample):
        nonlocal baseline, expected, overlay_hwnds
        current = g.snapshot()["profile"]
        if baseline is None:
            if args.continuation or args.reveal_only:
                assert current == profile, "Fresh continuation did not preserve exact accepted seed"
            baseline = copy.deepcopy(current)
            expected = copy.deepcopy(current)
        # Each known action has one immutable before/candidate pair. Retrying
        # accepts only that exact prior state as lag; unrelated changes fail
        # immediately. The caller's original finite sample deadline is retained.
        changes = {"wheel-choice": {"horizontal_wheel": "preserve"},
            "overlay-choice": {"idle_hide_ms": 5000},
            "error-stage": {"bounds_mode": "monitor", "monitor_id": "OWN-UNAVAILABLE-DISPLAY"},
            "restore-bounds": {"bounds_mode": "legacy"}, "choice": {"shape": "crosshair"}}
        if name in changes:
            if name not in transitions:
                candidate = copy.deepcopy(expected)
                candidate["seats"][0]["cursor"].update(changes[name])
                transitions[name] = {"before": copy.deepcopy(expected), "candidate": candidate,
                    "started": time.monotonic(), "stale_samples": 0}
            transition = transitions[name]
            if current == transition["before"] and current != transition["candidate"]:
                transition["stale_samples"] += 1
                g.evidence.setdefault("known_transition_waits", {})[name] = {
                    "stale_samples": transition["stale_samples"],
                    "observed_values": {k: current["seats"][0]["cursor"][k] for k in changes[name]},
                    "candidate_values": changes[name], "deadline": "unchanged original sample deadline"}
                if transition["stale_samples"] == 1:
                    g.save()
                return False
            assert current == transition["candidate"], "Unrelated mutation during known transition"
            expected = copy.deepcopy(transition["candidate"])
            sample["known_transition"] = {"stale_samples": transition["stale_samples"],
                "elapsed_seconds": time.monotonic()-transition["started"],
                "accepted_values": changes[name]}
        else:
            assert current == expected, "Unexpected draft transfer/mutation across seat or tab"
        if args.phase == "offline":
            if args.reveal_only:
                if name == "configure" and not g.initial_frame("configure-initial540", [540,520]):
                    return False
                capture_name = {"configure":"reveal-start", "reveal-wheel":"reveal-wheel-minimum",
                    "reveal-overlay":"reveal-overlay-minimum", "reveal-lock":"reveal-lock-minimum",
                    "invalid-footer":"validation-footer-minimum"}.get(name)
                if capture_name and not g.frame(capture_name, [320,360]):
                    return False
                sample["profile_equals_continuation_seed"] = current == profile
                if name in {"reveal-wheel", "reveal-overlay", "reveal-lock"}:
                    assert current == profile, "Same-value reveal changed complete profile"
            elif name in {"input", "wheel-choice", "overlay-choice", "lock"} or (args.continuation and name == "wheel"):
                if not g.pair({"wheel-choice":"wheel", "overlay-choice":"overlay"}.get(name, name)):
                    return False
            if name == "invalid-footer" and not g.frame("validation-footer-minimum", [320,360]):
                return False
            if name == "seat-2-overlay" and not g.frame("seat-2-values", [680,620]):
                return False
            if name == "seat-1-wheel" and not g.frame("seat-1-restored-values", [680,620]):
                return False
        else:
            if name == "start":
                if len(sample["state"]) != 1:
                    return False
                overlay_hwnds = [sample["state"][0]["overlay"]["hwnd"]]
            if name == "apply":
                if not sample["state"] or sample["state"][0]["overlay"]["shape"] != "crosshair":
                    return False
                assert all(ui.user32.IsWindow(h) for h in overlay_hwnds)
                if not g.frame("live-applied-footer", [320,360]):
                    return False
            if name in {"remove-disabled", "keyboard"}:
                if not g.frame("live-"+name, [320,360]):
                    return False
            if name == "stop":
                if any(ui.user32.IsWindow(h) for h in overlay_hwnds):
                    return False
                assert not sample["probes"][0]["held_keys"] and not sample["probes"][0]["buttons"]
        sample["profile_values_verified"] = True
        return True

    def before(gate):
        return gate.main_hwnd() is not None

    def after(gate):
        if args.reveal_only:
            assert gate.snapshot()["profile"] == profile, "Temporary validation draft not fully restored"
        gate.expected_exit = True
        assert geometry.user32.PostMessageW(gate.main_hwnd(), 0x10, 0, 0)
        gate.gui.wait(timeout=10)
        assert gate.gui.returncode == 0 and not hotkeys.lifecycle_window(gate.gui.pid)
        return True

    try:
        source = g.run/"runner-source"
        source.mkdir()
        shutil.copy2(Path(__file__), source/Path(__file__).name)
        g.evidence["scope"] = "Per-user layout/moved-Apply overlay only; x64 GUI, i686 copied unused; no new routed-input or release validation; independent original-pixel review required"
        g.execute(profile, prefs, steps, verify, before, after)
        g.evidence["captures"] = g.frames
        g.evidence["control_and_profile_result"] = "PASS"
        g.evidence["result"] = "PENDING INDEPENDENT VISUAL REVIEW"
        g.save()
        return g.run
    finally:
        g.close()
        lifecycle.remove_startup_key(g.startup_key)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready", action="store_true")
    parser.add_argument("--phase", choices=("offline", "live"), required=True)
    parser.add_argument("--continuation", action="store_true")
    parser.add_argument("--reveal-only", action="store_true")
    parser.add_argument("--from-run", type=Path)
    for name in ("executable", "x86-executable", "runtime"):
        parser.add_argument("--"+name, type=Path, required=True)
    args = parser.parse_args()
    assert args.ready, "Await frozen images AND exclusive acceptance slot"
    assert not (args.continuation and args.reveal_only)
    assert bool(args.from_run) == (args.continuation or args.reveal_only), "Continuation/reveal requires exact prefix provenance"
    workflow(args)


if __name__ == "__main__":
    main()
