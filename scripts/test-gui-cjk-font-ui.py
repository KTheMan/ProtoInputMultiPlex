"""Finite installed CJK GUI font comparison; explicit frozen READY required."""
import argparse
import copy
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import shutil
import time


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def own_process_sample(pid):
    """Query only the caller-created GUI PID, once; informational, not a benchmark."""
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    psapi = ctypes.WinDLL("psapi", use_last_error=True)
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.GetProcessTimes.argtypes = [wintypes.HANDLE]+[ctypes.POINTER(wintypes.FILETIME)]*4
    class Memory(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)]+[
            (name, ctypes.c_size_t) for name in (
                "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage", "QuotaPagedPoolUsage",
                "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage", "PagefileUsage", "PeakPagefileUsage", "PrivateUsage")]
    psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(Memory), wintypes.DWORD]
    handle = kernel.OpenProcess(0x1000 | 0x10, False, pid)
    assert handle
    try:
        times = [wintypes.FILETIME() for _ in range(4)]
        assert kernel.GetProcessTimes(handle, *(ctypes.byref(value) for value in times))
        created = ((times[0].dwHighDateTime << 32) | times[0].dwLowDateTime)/10_000_000-11_644_473_600
        memory = Memory()
        memory.cb = ctypes.sizeof(memory)
        assert psapi.GetProcessMemoryInfo(handle, ctypes.byref(memory), memory.cb)
        return {"pid": pid, "process_creation_to_first_observed_complete_product_png_ms": round((time.time()-created)*1000, 2),
                "working_set_bytes": memory.WorkingSetSize, "private_bytes": memory.PrivateUsage,
                "scope": "One fresh-process observed sample; cache state unknown; includes screenshot generation/observation delay, not exact first presentation or statistical performance"}
    finally:
        kernel.CloseHandle(handle)


def workflow(args, label, baseline_profile=None):
    lifecycle = module("font_lifecycle", "test-app-lifecycle-ui.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    from PIL import Image
    class Gate(Base):
        def __init__(self):
            super().__init__(args, "gui-cjk-"+label, offline=True, count=0)
            self.frames = {}
            self.requested = {}

        def frame(self, name, size):
            if name in self.frames:
                return True
            hwnd = self.main_hwnd()
            if name not in self.requested:
                geometry.resize_owned_client(hwnd, size)
                self.requested[name] = time.time()
                return False
            current = geometry.geometry(hwnd)
            if any(abs(a-b)>2 for a, b in zip(current["client_logical"], size)):
                return False
            shot = geometry.copy_owned_product_frame(self.run/"ui.png", self.run/(name+".png"),
                                                     hwnd, self.requested[name])
            if not shot:
                return False
            self.frames[name] = shot
            return True

    g = Gate()
    # Long enough for roster truncation, but sized to expose the full text in
    # the actual existing 540-point Input field without claiming a hover.
    names = ["Owned Latin", "漢字かなカナ", "한글 사용자", "長い名前漢字かなカナ"*2+" 🙂🖱", "Owned Caret"]
    profile = {"version": 1, "concurrent": True, "desktop": None,
               "seats": [{"id": f"font-owned-{i}", "name": name,
                          "mouse": f"virtual://mouse/{i+1}", "keyboard": f"virtual://keyboard/{i+1}",
                          "target_pid": 987201+i,
                          "cursor": {"color": [[70,190,160], [230,130,70], [80,160,230], [190,90,210], [200,180,60]][i]}}
                         for i, name in enumerate(names)]}
    prefs = {"version": 1, "appearance_mode": "dark", "zoom_percent": 100,
             "launch_minimized": False, "minimize_to_tray": False, "close_to_tray": False,
             "launch_at_login": False, "shortcuts": []}
    step = ui.step
    steps = [step("select-long", "Select user 4")]
    if label == "candidate":
        steps += [step("configure", "Configure user"), step("buttons", "Buttons"),
                  step("home", "Back to Home"), step("settings", "Settings"),
                  step("preferences", "Application preferences"), step("light", "Appearance mode Light"),
                  step("apply", "Apply application preferences"), step("home-light", "Back to Home")]
    expected = None
    font = Path(os.environ.get("WINDIR", "C:/Windows"))/"Fonts/malgun.ttf"

    def before(gate):
        hwnd = gate.main_hwnd()
        if not hwnd:
            return False
        path = gate.run/"ui.png"
        try:
            before_stat = path.stat()
            data = path.read_bytes()
            after_stat = path.stat()
            if (before_stat.st_size, before_stat.st_mtime_ns) != (after_stat.st_size, after_stat.st_mtime_ns):
                return False
            with Image.open(io.BytesIO(data)) as image:
                image.load()
                if list(image.size) != geometry.geometry(hwnd)["client_pixels"]:
                    return False
        except (OSError, ValueError):
            return False
        gate.evidence["startup_information"] = own_process_sample(gate.gui.pid)
        return True

    def verify(name, sample):
        nonlocal expected
        current = g.snapshot()["profile"]
        if expected is None:
            expected = copy.deepcopy(current)
            if baseline_profile is not None:
                assert current == baseline_profile, "Font candidate changed normalized profile"
        assert current == expected, "Navigation/theme/font changed profile"
        assert [seat["name"] for seat in current["seats"]] == names, "Full Unicode fixture text changed"
        assert not (g.run/"ui.state.json").exists(), "Offline font gate created worker"
        sample["complete_profile_unchanged"] = True
        if name == "select-long":
            if not g.frame("home-dark-normal", [360, 540]):
                return False
            if label == "candidate" and not g.frame("home-dark-minimum", [320, 360]):
                return False
        if name == "configure" and not g.frame("input-long-normal", [540, 520]):
            return False
        if name == "buttons" and not g.frame("buttons-caret-minimum", [320, 360]):
            return False
        if name in {"apply", "home-light"}:
            actual = ui.read_json(g.preferences_path)
            after = {**prefs, "appearance_mode": "light"}
            if actual == prefs:
                return False
            assert actual == after, "Theme Apply changed unrelated preferences"
        if name == "home-light" and not g.frame("home-light-normal", [360, 540]):
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
        g.evidence["scope"] = "Stopped owned GUI installed-font fallback only; no worker/DLL/MCP/probes/native-cursor font or all-Unicode/locale/performance claim; i686 unused"
        g.evidence["installed_font"] = {"path": str(font), "exists": font.exists(),
            "bytes": font.stat().st_size if font.exists() else None,
            "sha256": hashlib.sha256(font.read_bytes()).hexdigest() if font.exists() else None}
        g.execute(profile, prefs, steps, verify, before, after)
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
    assert args.ready, "Await frozen source/images and exclusive slot"
    assert hashlib.sha256(args.baseline_executable.read_bytes()).hexdigest() == "ed07efad77ecb2385c4d93c7d90c000be3d33a65d588ee80bc539cdbcaf019e4"
    baseline_args = copy.copy(args)
    baseline_args.executable = args.baseline_executable
    baseline_run, profile = workflow(baseline_args, "baseline")
    candidate_run, _ = workflow(args, "candidate", profile)
    print(f"PENDING direct before/after glyph review: {baseline_run} ; {candidate_run}", flush=True)


if __name__ == "__main__":
    main()
