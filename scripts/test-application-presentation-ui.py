"""Focused real-control presentation tail. Preparation is AST-only until READY.

Immutable owned-app captures require a separate independent review after the
finite interactions and cleanup. Geometry alone never certifies visual quality.
"""
import argparse
import copy
import ctypes
import hashlib
import importlib.util
from pathlib import Path
import shutil
import time


def module(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def workflow(args):
    lifecycle = module("presentation_ui_lifecycle", "test-app-lifecycle-ui.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()

    class PresentationGate(Base):
        def __init__(self):
            super().__init__(args, "application-presentation-" + args.phase, offline=True, count=0)
            self.captures = {}
            self.capture_after = {}

        def main_hwnd(self):
            # At75% Home is270px wide; the older lifecycle helper's300px
            # startup filter must not hide a valid zoomed owned viewport.
            matches = []
            @geometry.callback_type
            def visit(hwnd, unused):
                pid = lifecycle.wintypes.DWORD()
                geometry.user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
                if pid.value != self.gui.pid or geometry.user32.GetWindow(hwnd, 4):
                    return True
                title = ctypes.create_unicode_buffer(64)
                geometry.user32.GetWindowTextW(hwnd, title, len(title))
                rect = geometry.Rect()
                if title.value == "ProtoInputMultiPlex" and geometry.user32.GetClientRect(hwnd, ctypes.byref(rect)):
                    if rect.right > 100 and rect.bottom > 100:
                        matches.append(int(hwnd))
                return True
            assert geometry.user32.EnumWindows(visit, 0)
            assert len(matches) <= 1
            return matches[0] if matches else None

        def frame(self, name, points=None, zoom=100, after=0):
            if name in self.captures:
                return self.captures[name]
            self.capture_after.setdefault(name, time.time())
            hwnd = self.main_hwnd()
            if not hwnd:
                return None
            observed = geometry.geometry(hwnd)
            observed["app_zoom_percent"] = zoom
            observed["client_points"] = [p * 100 / zoom for p in observed["client_logical"]]
            if points and any(abs(a - b) > 2 for a, b in zip(observed["client_points"], points)):
                return None
            shot = geometry.copy_owned_product_frame(self.run / "ui.png", self.run / (name + ".png"),
                hwnd, max(after, self.capture_after[name]))
            if shot:
                shot["geometry"] = observed
                self.captures[name] = shot
                self.evidence["owned_images"] = self.captures
                self.save()
            return shot

        def committed(self, expected):
            page = self.app_snapshot("ui.app-preferences.json")
            return page and page.get("committed") == expected and ui.read_json(self.preferences_path) == expected

    g = PresentationGate()
    long = args.phase in ("long-home", "options-tail")
    replay = args.phase in ("relaunch", "selection-only")
    continuation = args.phase == "zoom-tail"
    names = ["Owned Alpha", "Owned Beta"] if not long else [
        "Owned long 東京🙂 " + "WideName " * 12,
        *[f"Owned user {i}" for i in range(2, 12)], "Owned LAST12"]
    palette = ((96, 165, 250), (239, 68, 68), (34, 197, 94), (94, 234, 212), (245, 158, 11), (212, 212, 216))
    profile = {"version": 1, "concurrent": True, "seats": [{"id": f"presentation-own-{i}",
        "name": name, "mouse": f"virtual://mouse/{i+1}", "keyboard": f"virtual://keyboard/{i+1}",
        "target_pid": 987001+i, "cursor": {"color": list(palette[i % 6])}} for i, name in enumerate(names)]}
    if long:
        # Offline synthetic scope only: expose the automatic-mode Radio view
        # without opening a backend or discovering unrelated target titles.
        profile["desktop"] = {"mode": "switched", "multi_keyboard": True,
            "allowed_pids": [987001+i for i in range(len(names))]}
    prefs = {"version": 1, "appearance_mode": "dark", "zoom_percent": 100,
        "launch_minimized": False, "minimize_to_tray": False, "close_to_tray": False, "launch_at_login": False,
        "shortcuts": [{"action": action, "key": key,
            "modifiers": {"control": True, "alt": True, "shift": True, "windows": False}}
            for action, key in (("stop", "f11"), ("pause_resume", "f6"))]}
    if replay or continuation:
        assert args.from_run, "Relaunch consumes the exact successful first-run preferences"
        args.from_run = args.from_run.resolve()
        previous = ui.read_json(args.from_run / "evidence.json")
        if replay:
            assert previous.get("control_and_geometry_result") == "PASS"
            g.evidence["relaunch_source_build_sha256"] = previous["build_sha256"]
            g.evidence["relaunch_image_scope"] = "Actual persisted document loaded by this exact current GUI; original creator image recorded separately, including cross-image presentation-only corrections."
        else:
            assert "apply-light" in previous["samples"], "Continuation must reference the accepted Light100 acknowledgement"
        prefs = ui.read_json(args.from_run / "owned preferences.json")
        assert prefs["appearance_mode"] == "light" and prefs["zoom_percent"] == (75 if replay else 100)
        g.evidence["relaunch_from"] = str(args.from_run)
        if replay:
            g.preferences_path = args.from_run / "owned preferences.json"
        else:
            source_bytes = (args.from_run / "owned preferences.json").read_bytes()
            g.preferences_path.write_bytes(source_bytes)
            g.evidence["continuation_document"] = {"source": str(args.from_run / "owned preferences.json"),
                "sha256": hashlib.sha256(source_bytes).hexdigest(), "initial_copy_byte_exact": True,
                "draft90_setup": "Explicit field staging on the plus10 step; not persisted state",
                "source_build_sha256": previous["build_sha256"]}
    persisted_bytes = g.preferences_path.read_bytes() if replay else None
    zoom = prefs["zoom_percent"]
    step = ui.step
    steps = [step("expanded", "Routing options"), step("collapsed", "Routing options")]
    if args.phase == "selection-only":
        steps = [step("select2", "Select user 2")]
    if long:
        # Selected Switched is a no-op real Radio click that leaves the top
        # options viewport stable for its capture before Multi auto-scrolls.
        steps = [step("expanded", "Routing options"), step("switched-view", "Switched"),
            step("multi-off", "Multi keyboard"), step("multi-on", "Multi keyboard"),
            step("collapsed", "Routing options")]
        if args.phase == "options-tail":
            steps = [s for s in steps if s["id"] != "switched-view"]
    if continuation:
        steps += [step("settings", "Settings"), step("preferences", "Application preferences"),
            step("plus", "Zoom plus 10", {"app_zoom_percent": 90}),
            step("preset75", "Zoom preset 75%"), step("apply75", "Apply application preferences"),
            step("home75", "Back to Home")]
    elif not long and not replay:
        steps += [step("configure", "Configure user"), step("back", "Back to Home"),
            step("settings", "Settings"), step("preferences", "Application preferences"),
            step("wide-edit", "Shortcut Stop routing"), step("wide-restore", "Shortcut Stop routing"),
            step("dark", "Appearance mode Dark"), step("light", "Appearance mode Light"),
            step("apply-light", "Apply application preferences"), step("minus", "Zoom minus 10"),
            step("plus", "Zoom plus 10"), step("preset75", "Zoom preset 75%"),
            step("apply75", "Apply application preferences"), step("home75", "Back to Home")]
    steps[0]["wait_ms"] = 12000
    baseline = None
    minimum_started = 0
    physical_before_zoom = None

    def before(gate):
        nonlocal baseline, minimum_started
        default = (360, 540) if long else (360, 360)
        if not gate.frame("home-default", default, zoom):
            return False
        if baseline is None:
            baseline = copy.deepcopy(gate.snapshot()["profile"])
            assert ui.read_json(gate.preferences_path) == prefs
            geometry.resize_owned_client(gate.main_hwnd(), (320*zoom/100, 360*zoom/100))
            minimum_started = time.time()
        return bool(gate.frame("home-minimum", (320, 360), zoom, minimum_started))

    def verify(name, sample):
        nonlocal physical_before_zoom
        assert not (g.run / "ui.state.json").exists(), "Offline presentation started a worker"
        expected_profile = copy.deepcopy(baseline)
        if name == "multi-off":
            expected_profile["desktop"]["multi_keyboard"] = False
        assert sample["profile_state"]["profile"] == expected_profile
        if name == "expanded":
            return bool(g.frame("home-expanded", (320, 360), zoom))
        if name == "select2":
            shot = g.frame("selected-user2", (320, 360), zoom)
            if not shot:
                return False
            sample["actual_selected_row_image"] = shot
            sample["selection_visual_criterion"] = "Independent exact-original review must confirm selected styling moved from row1 to row2; profile JSON does not expose transient selection."
        if name == "multi-off":
            return bool(g.frame("home-options-scroll-end", (320, 360), zoom))
        if name == "multi-on":
            return bool(g.frame("home-options-multi-restored", (320, 360), zoom))
        if name == "configure":
            return bool(g.frame("configure-full-width", (680, 620)))
        if name == "back":
            return bool(g.frame("home-back", (360, 360)))
        if name == "wide-edit":
            return bool(g.frame("preferences-wide-alignment", (680, 620)))
        if name == "apply-light":
            if not g.committed({**prefs, "appearance_mode": "light"}):
                return False
            return bool(g.frame("preferences-light100", (680, 620)))
        if name in ("minus", "plus", "preset75"):
            page = g.app_snapshot("ui.app-preferences.json")
            expected = {"minus": 90, "plus": 100, "preset75": 75}[name]
            if not page or page["draft"]["zoom_percent"] != expected:
                return False
            if name == "preset75":
                physical_before_zoom = geometry.geometry(g.main_hwnd())["client_pixels"]
        if name == "apply75":
            if not g.committed({**prefs, "appearance_mode": "light", "zoom_percent": 75}):
                return False
            shot = g.frame("preferences-light75-runtime", zoom=75)
            if not shot:
                return False
            assert shot["geometry"]["client_pixels"] == physical_before_zoom, "Runtime zoom unexpectedly resized a sufficiently large physical client"
        if name == "home75":
            return bool(g.frame("home-light75", (360, 360), 75))
        return True

    def after(gate):
        gate.expected_exit = True
        assert geometry.user32.PostMessageW(gate.main_hwnd(), 0x10, 0, 0)
        gate.gui.wait(timeout=10)
        assert gate.gui.returncode == 0 and not hotkeys.lifecycle_window(gate.gui.pid)
        if replay:
            assert gate.preferences_path.read_bytes() == persisted_bytes, "Relaunch altered the accepted preferences"
        return True

    try:
        source = g.run / "runner-source"
        source.mkdir()
        shutil.copy2(Path(__file__), source / Path(__file__).name)
        shutil.copy2(Path(__file__).with_name("owned-gui-geometry.py"), source / "owned-gui-geometry.py")
        g.evidence["scope"] = "Owned offline x64 GUI only; copied i686 unused; independent visuals pending"
        command = None
        if replay or continuation:
            # Consume the accepted file in place; do not rewrite it as a fresh
            # fixture and then claim that demonstrates persisted loading.
            command = f'"{g.images[0]}" gui --profile "{g.run / "profile.json"}" --preferences "{g.preferences_path}"'
        g.execute(profile, prefs, steps, verify, before, after, launch_command=command)
        g.evidence["control_and_geometry_result"] = "PASS"
        g.evidence["result"] = "PENDING INDEPENDENT VISUAL REVIEW"
        g.save()
        return g.run
    finally:
        g.close()
        lifecycle.remove_startup_key(g.startup_key)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready", action="store_true")
    for name in ("executable", "x86-executable", "runtime"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--phase", choices=("home-preferences", "zoom-tail", "long-home", "options-tail", "relaunch", "selection-only"), required=True)
    parser.add_argument("--from-run", type=Path)
    args = parser.parse_args()
    assert args.ready, "Await frozen image and exclusive acceptance slot"
    workflow(args)


if __name__ == "__main__":
    main()
