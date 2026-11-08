"""Coordinated GUI launch smoke test; no pointer, keyboard or routing actions.

Reserve the main session's native test slot before running this script.
Screenshots come only from the application's opt-in egui test hook.
"""
import argparse
import ctypes
from ctypes import wintypes
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import traceback
import uuid


def image_path(kernel, handle):
    buffer = ctypes.create_unicode_buffer(32768)
    length = wintypes.DWORD(len(buffer))
    if kernel.QueryFullProcessImageNameW(handle, 0, buffer, ctypes.byref(length)):
        return Path(buffer.value).resolve()
    return None


def owned_processes(executable, terminate=False):
    """Find exact unique bundle image paths, including hidden/windowless orphans."""
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    class Entry(ctypes.Structure):
        _fields_ = [("size", wintypes.DWORD), ("usage", wintypes.DWORD),
                    ("pid", wintypes.DWORD), ("heap", ctypes.c_size_t),
                    ("module", wintypes.DWORD), ("threads", wintypes.DWORD),
                    ("parent", wintypes.DWORD), ("priority", wintypes.LONG),
                    ("flags", wintypes.DWORD), ("exe", wintypes.WCHAR*260)]
    kernel.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
    kernel.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    kernel.Process32FirstW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Entry)]
    kernel.Process32NextW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Entry)]
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.QueryFullProcessImageNameW.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD)]
    kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
    kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    snapshot = kernel.CreateToolhelp32Snapshot(2, 0)
    if snapshot == wintypes.HANDLE(-1).value: raise ctypes.WinError(ctypes.get_last_error())
    found = []
    try:
        entry = Entry()
        entry.size = ctypes.sizeof(Entry)
        more = kernel.Process32FirstW(snapshot, ctypes.byref(entry))
        while more:
            if entry.exe.lower() == executable.name.lower():
                handle = kernel.OpenProcess(0x101001 if terminate else 0x1000, False, entry.pid)
                if handle:
                    try:
                        # The queried and terminated process is the same handle, avoiding PID reuse.
                        if image_path(kernel, handle) == executable.resolve():
                            found.append(entry.pid)
                            if terminate:
                                if not kernel.TerminateProcess(handle, 1): raise ctypes.WinError(ctypes.get_last_error())
                                kernel.WaitForSingleObject(handle, 5000)
                    finally:
                        kernel.CloseHandle(handle)
            more = kernel.Process32NextW(snapshot, ctypes.byref(entry))
    finally:
        kernel.CloseHandle(snapshot)
    return found


def owned_windows(executable):
    user = ctypes.WinDLL("user32", use_last_error=True)
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.QueryFullProcessImageNameW.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD)]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    user.IsWindowVisible.argtypes = [wintypes.HWND]
    user.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
    result = []
    callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    def callback(window, _):
        if not user.IsWindowVisible(window): return True
        pid = wintypes.DWORD()
        user.GetWindowThreadProcessId(window, ctypes.byref(pid))
        handle = kernel.OpenProcess(0x1000, False, pid.value)
        if handle:
            try:
                buffer = ctypes.create_unicode_buffer(32768)
                length = wintypes.DWORD(len(buffer))
                if kernel.QueryFullProcessImageNameW(handle, 0, buffer, ctypes.byref(length)):
                    if Path(buffer.value).resolve() == executable.resolve():
                        result.append((int(window), pid.value))
            finally:
                kernel.CloseHandle(handle)
        return True
    callback_object = callback_type(callback)
    user.EnumWindows.argtypes = [callback_type, wintypes.LPARAM]
    user.EnumWindows(callback_object, 0)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="New evidence directory")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    report = {"result": "FAIL", "routing_validation": "NOT RUN", "input_emitted": False, "cases": []}
    try:
        with tempfile.TemporaryDirectory(prefix="ProtoInputMultiPlex GUI portable ") as temporary:
            root = Path(temporary)
            unrelated = root/"unrelated cwd"
            unrelated.mkdir()
            env = dict(os.environ)
            system_root = Path(os.environ.get("SystemRoot", r"C:\Windows"))
            command_shell = system_root/"System32/cmd.exe"
            env["PATH"] = os.pathsep.join([str(system_root/"System32"), str(system_root)])
            env["LOCALAPPDATA"] = str(root/"unused local app data")
            for name in ("runtime-present", "dll-free"):
                bundle = root/name/"bundle with spaces"
                shutil.copytree(args.bundle, bundle)
                if name == "dll-free":
                    shutil.move(str(bundle/"runtime"), str(root/"runtime removed"))
                preferences = bundle/"data/app-preferences.json"
                preferences.parent.mkdir(exist_ok=True)
                preferences.write_text(json.dumps({"version": 1, "shortcuts": [], "launch_at_login": False,
                                                   "close_to_tray": False, "launch_minimized": False,
                                                   "appearance_mode": "light", "zoom_percent": 75}), encoding="utf-8")
                before = hashlib.sha256(preferences.read_bytes()).hexdigest()
                script = root/(name+".json")
                script.write_text("[]", encoding="utf-8")
                test_key = "Software\\ProtoInputMultiPlex\\Tests\\Portable-"+uuid.uuid4().hex
                command = f'"{bundle / "launch-gui.cmd"}" --ui-script "{script}" --startup-test-key "{test_key}"'
                case = {"name": name, "bundle": str(bundle), "cwd": str(unrelated), "startup_test_key": test_key}
                report["cases"].append(case)
                with (args.output/(name+".stdout.txt")).open("w") as stdout, (args.output/(name+".stderr.txt")).open("w") as stderr:
                    process = subprocess.Popen(f'"{command_shell}" /d /s /c "{command}"', cwd=unrelated,
                                               env=env, stdout=stdout, stderr=stderr, creationflags=subprocess.CREATE_NO_WINDOW)
                    windows = []
                    try:
                        deadline = time.monotonic()+35
                        while time.monotonic() < deadline:
                            windows = owned_windows(bundle/"protoinputmultiplex.exe")
                            if windows and all(script.with_suffix(s).exists() for s in (".png", ".profile.json")): break
                            if process.poll() is not None: raise AssertionError(f"GUI exited early: {process.returncode}")
                            time.sleep(0.2)
                        else: raise AssertionError("Owned visible GUI and screenshot/state did not become ready")
                        case["windows"] = [{"hwnd": hwnd, "pid": pid} for hwnd, pid in windows]
                        # Home does not emit the Settings-page preferences snapshot.
                        prefs_snapshot = script.with_suffix(".app-preferences.json")
                        if prefs_snapshot.exists():
                            captured_preferences = json.loads(prefs_snapshot.read_text(encoding="utf-8"))
                            if Path(captured_preferences["path"]).resolve() != preferences.resolve(): raise AssertionError("GUI preference path escaped portable data")
                            if captured_preferences["committed"]["shortcuts"] != []: raise AssertionError("GUI registered unexpected shortcut preferences")
                            if captured_preferences["committed"]["launch_at_login"]: raise AssertionError("Unexpected startup preference")
                        case["preferences_path"] = str(preferences)
                        case["seeded_preferences"] = json.loads(preferences.read_text(encoding="utf-8"))
                        case["settings_snapshot_emitted"] = prefs_snapshot.exists()
                        state_path = script.with_suffix(".state.json")
                        # Stopped GUI emits no routing state events at all.
                        if state_path.exists() and json.loads(state_path.read_text(encoding="utf-8")) != []:
                            raise AssertionError("GUI unexpectedly reported active routing states")
                        if len(owned_processes(bundle/"protoinputmultiplex.exe")) != 1:
                            raise AssertionError("Unexpected owned worker process during launch-only test")
                        case["routing_state_emitted"] = state_path.exists()
                        for suffix in (".png", ".state.json", ".app-preferences.json", ".profile.json"):
                            source = script.with_suffix(suffix)
                            if source.exists(): shutil.copyfile(source, args.output/(name+suffix))
                        user = ctypes.WinDLL("user32")
                        user.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
                        for window, _ in windows:
                            if not user.PostMessageW(window, 0x10, 0, 0): raise AssertionError("Owned WM_CLOSE failed")
                        process.wait(timeout=15)
                        if process.returncode != 0: raise AssertionError(f"GUI wrapper exit {process.returncode}")
                        if owned_windows(bundle/"protoinputmultiplex.exe"): raise AssertionError("Owned window survived close")
                        if owned_processes(bundle/"protoinputmultiplex.exe"): raise AssertionError("Owned process survived close")
                        if hashlib.sha256(preferences.read_bytes()).hexdigest() != before: raise AssertionError("Preferences changed without a save")
                        if Path(env["LOCALAPPDATA"]).exists(): raise AssertionError("Portable GUI wrote LocalAppData")
                        case.update(result="PASS", clean_close=True, preferences_unchanged=True)
                    finally:
                        # Clean hidden/orphaned processes independently of wrapper lifetime.
                        forced = owned_processes(bundle/"protoinputmultiplex.exe", terminate=True)
                        case["forced_cleanup_pids"] = forced
                        if process.poll() is None:
                            process.kill()
                            process.wait(timeout=10)
                        survivors = owned_processes(bundle/"protoinputmultiplex.exe")
                        case["remaining_owned_pids"] = survivors
                        if survivors: raise AssertionError(f"Owned process cleanup failed: {survivors}")
        report["result"] = "PASS"
    except Exception:
        report["error"] = traceback.format_exc()
    (args.output/"verification.json").write_text(json.dumps(report, indent=2)+"\n", encoding="utf-8")
    print(json.dumps({"result": report["result"], "evidence": str(args.output.resolve())}))
    if report["result"] != "PASS": raise SystemExit(1)


if __name__ == "__main__":
    main()
