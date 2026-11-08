"""Actual portable routing acceptance. Reserve the main native slot before use.

Only virtual devices and owned disposable probe PIDs receive input. No OS input
injection, global pointer control, GUI preference changes, or desktop capture.
"""
import argparse
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import tempfile
import time
import traceback
import zipfile


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


base = load("portable_headless", "test-portable.py")
gui = load("portable_ownership", "test-portable-gui.py")
sha = base.sha256
require = base.require
user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.SetWindowPos.argtypes = [wintypes.HWND, wintypes.HWND, ctypes.c_int,
                              ctypes.c_int, ctypes.c_int, ctypes.c_int, wintypes.UINT]
user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
user32.IsWindow.argtypes = [wintypes.HWND]
kernel = ctypes.WinDLL("kernel32", use_last_error=True)
kernel.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
kernel.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
kernel.CloseHandle.argtypes = [wintypes.HANDLE]


def scoped_processes(root, terminate=False):
    """Find every process under this unique disposable root, including injectors."""
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
    require(snapshot != wintypes.HANDLE(-1).value, "Process snapshot failed")
    found = []
    try:
        entry = Entry()
        entry.size = ctypes.sizeof(Entry)
        more = kernel.Process32FirstW(snapshot, ctypes.byref(entry))
        while more:
            handle = kernel.OpenProcess(0x101001 if terminate else 0x1000, False, entry.pid)
            if handle:
                try:
                    path = gui.image_path(kernel, handle)
                    if path and path.is_relative_to(root.resolve()):
                        found.append({"pid": entry.pid, "parent_pid": entry.parent, "image": str(path)})
                        if terminate:
                            # Revalidate ownership on this same process handle before mutation.
                            require(gui.image_path(kernel, handle) == path, "Process identity changed")
                            require(kernel.TerminateProcess(handle, 1), "Owned termination failed")
                            kernel.WaitForSingleObject(handle, 5000)
                finally:
                    kernel.CloseHandle(handle)
            more = kernel.Process32NextW(snapshot, ctypes.byref(entry))
    finally:
        kernel.CloseHandle(snapshot)
    return found


def modules(pid):
    class Module(ctypes.Structure):
        _fields_ = [("size", wintypes.DWORD), ("id", wintypes.DWORD),
                    ("pid", wintypes.DWORD), ("global_usage", wintypes.DWORD),
                    ("process_usage", wintypes.DWORD), ("base", ctypes.c_void_p),
                    ("bytes", wintypes.DWORD), ("handle", wintypes.HMODULE),
                    ("name", wintypes.WCHAR*256), ("path", wintypes.WCHAR*260)]
    kernel.Module32FirstW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Module)]
    kernel.Module32NextW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Module)]
    snapshot = kernel.CreateToolhelp32Snapshot(0x18, pid)
    require(snapshot != wintypes.HANDLE(-1).value, f"Module snapshot failed for {pid}: {ctypes.get_last_error()}")
    result = []
    try:
        entry = Module()
        entry.size = ctypes.sizeof(Module)
        more = kernel.Module32FirstW(snapshot, ctypes.byref(entry))
        require(more, f"Module enumeration failed for {pid}")
        while more:
            result.append({"name": entry.name, "path": entry.path})
            more = kernel.Module32NextW(snapshot, ctypes.byref(entry))
    finally:
        kernel.CloseHandle(snapshot)
    return result


class Client(base.Mcp):
    def request(self, method, params=None):
        # The app has a bounded50s worker acknowledgement; allow it to report it.
        self.counter += 1
        identity = self.counter
        self.methods.append(method)
        self.send({"jsonrpc": "2.0", "id": identity, "method": method, "params": params or {}})
        deadline = time.monotonic()+75
        while True:
            message = self.messages.get(timeout=max(.01, deadline-time.monotonic()))
            if isinstance(message, Exception):
                raise RuntimeError(f"{message}; stderr: {''.join(self.errors)}")
            if message.get("id") == identity:
                require("error" not in message, f"{method}: {message.get('error')}")
                return message["result"]
            require(time.monotonic() < deadline, f"{method} timed out")


def eventually(function, predicate, timeout=15):
    deadline = time.monotonic()+timeout
    last = None
    while time.monotonic() < deadline:
        try:
            last = function()
            if predicate(last): return last
        except (FileNotFoundError, json.JSONDecodeError):
            pass  # Probe saves are non-atomic; require a later complete fresh frame.
        time.sleep(.1)
    raise AssertionError(f"Condition timeout; last={last}")


def phase(bundle, fixture, scratch, env, unrelated, output, termination, report):
    case = {"name": termination, "result": "FAIL", "observations": {}, "checks": []}
    report["phases"].append(case)
    run = output/termination
    run.mkdir()
    exe = bundle/"protoinputmultiplex.exe"
    probes, streams, client = [], [], None
    allowed = []
    last_frames = [0, 0]
    paths = [run/"probe-x64.json", run/("probe-x86.json" if fixture else "probe-x64-second.json")]

    def note(name, value):
        case["observations"][name] = value
        case["checks"].append(name)
        print(f"PASS {termination}: {name}", flush=True)
        return value

    def fresh(predicate=lambda _: True):
        def read():
            return [json.loads(path.read_text(encoding="utf-8")) for path in paths]
        # Observe both peers advancing by >=2 frames after the command/baseline,
        # and a further report interval for negative no-crossover observations.
        first = eventually(read, lambda rs: all(r["pid"] == pid and r["frames"] > frame+2
                                               for r, pid, frame in zip(rs, allowed, last_frames)) and predicate(rs))
        sampled = [r["frames"] for r in first]
        final = eventually(read, lambda rs: all(r["pid"] == pid and r["frames"] > frame+2
                                               for r, pid, frame in zip(rs, allowed, sampled)) and predicate(rs))
        last_frames[:] = [r["frames"] for r in final]
        return final

    def action(kind, **fields):
        value = client.call("routing_input", {"action": kind, **fields})
        require(value.get("last_error") is None, f"Input error: {value}")
        return value

    def both_released(rs): return all(r["buttons"] == 0 and r["test_keys"] == 0 for r in rs)
    def both_held(rs): return rs[0]["buttons"] == rs[1]["buttons"] == 1 and rs[0]["test_keys"] == 1 and rs[1]["test_keys"] == 8

    try:
        for index, image in enumerate((exe, fixture or exe)):
            log = (run/f"probe-{index}.stderr.txt").open("w", encoding="utf-8")
            streams.append(log)
            probes.append(subprocess.Popen([str(image), "probe", "--report", str(paths[index])],
                                           cwd=unrelated, env=env, stdout=log, stderr=log,
                                           creationflags=subprocess.CREATE_NO_WINDOW))
        allowed[:] = [p.pid for p in probes]
        case["allowed_pids"] = allowed.copy()
        command_shell = Path(os.environ.get("SystemRoot", r"C:\Windows"))/"System32/cmd.exe"
        command = f'"{command_shell}" /d /s /c ""{bundle / "launch-mcp.cmd"}""'
        client = Client(command, unrelated, env)
        client.initialize()
        def inventory():
            return [w for w in client.call("routing_windows")["windows"] if w["pid"] in allowed
                    and w["title"].startswith("ProtoInputMultiPlex Routing Test")]
        windows = eventually(inventory, lambda ws: all(any(w["pid"] == p for w in ws) for p in allowed), 45)
        for index, pid in enumerate(allowed):
            window = next(w for w in windows if w["pid"] == pid)
            require(user32.SetWindowPos(window["hwnd"], wintypes.HWND(-1), 40+index*460, 50, 400, 360, 0x10), "Owned placement failed")
        windows = inventory()
        windows = [next(w for w in windows if w["pid"] == pid) for pid in allowed]
        points = [(w["client"]["left"]+140, w["client"]["top"]+120) for w in windows]
        case["owned_windows"] = windows
        for p, w in zip(points, windows):
            require(w["client"]["left"] < p[0] < w["client"]["right"] and w["client"]["top"] < p[1] < w["client"]["bottom"], "Target point outside owned client")
        note("targets_ready", fresh())
        client.call("routing_start", {"users": ["Denied"], "allowed_pids": [], "mode": "concurrent", "multi_keyboard": True}, True)
        started = client.call("routing_start", {"users": ["Portable Blue", "Portable Coral"],
                             "allowed_pids": allowed, "mode": "concurrent", "multi_keyboard": True})
        require(started["running"] and all(u["pid"] == 0 for u in started["users"]), "Unexpected initial target")
        for index, point in enumerate(points):
            action("move_to", user=index, x=point[0], y=point[1])
            selected = action("button", user=index, button="primary", pressed=True)
            require(selected["users"][index]["pid"] == allowed[index], "Wrong selected PID")
            fresh(lambda rs: rs[index]["buttons"] == 1 and rs[index]["mouse_clicks"] > 0)
            action("button", user=index, button="primary", pressed=False)
            fresh(both_released)
        note("native_selection_clicks", fresh(both_released))
        target_modules = []
        case["observations"]["target_module_audit"] = target_modules
        for index, pid in enumerate(allowed):
            name = "ProtoInputHooks32.dll" if index == 1 and fixture else "ProtoInputHooks64.dll"
            module = next(m for m in modules(pid) if m["name"].lower() == name.lower())
            actual = Path(module["path"])
            expected = bundle/"runtime"/name
            audit = {"pid": pid, **module, "expected": str(expected),
                     "sha256": sha(actual) if actual.is_file() else None}
            target_modules.append(audit)
            require(actual.is_file() and os.path.samefile(actual, expected), f"Hook loaded outside portable runtime: {audit}")
            require(audit["sha256"] == sha(expected), "Loaded hook bytes differ from portable payload")
        workers = []
        for process in scoped_processes(scratch):
            if process["pid"] in allowed or Path(process["image"]) != exe: continue
            matches = [m for m in modules(process["pid"]) if m["name"].lower() == "protoinputloader64.dll"]
            for module in matches:
                require(os.path.samefile(module["path"], bundle/"runtime/ProtoInputLoader64.dll"), "Loader loaded outside portable runtime")
                workers.append({**process, "loader": module, "sha256": sha(Path(module["path"]))})
        require(len(workers) == 1, "Expected one owned portable routing worker")
        note("bundled_native_modules_loaded", {"targets": target_modules, "workers": workers})

        if termination == "explicit-stop":
            client.call("routing_mode", {"mode": "switched", "multi_keyboard": True})
            baseline = fresh(both_released)
            action("button", user=0, button="primary", pressed=True)
            blocked = action("button", user=1, button="primary", pressed=True)
            require(blocked["users"][0]["root"] and not blocked["users"][1]["buttons"], "Switched root arbitration failed")
            action("key", user=1, key="d", pressed=True)
            observed = fresh(lambda rs: rs[0]["buttons"] == 1 and rs[1]["buttons"] == 0 and rs[0]["test_keys"] == 0 and rs[1]["test_keys"] == 8)
            require(observed[1]["mouse_clicks"] == baseline[1]["mouse_clicks"], "Blocked click reached peer")
            note("switched_arbitration_independent_keyboard", observed)
            action("release_all")
            note("release_all_while_hooks_active", fresh(both_released))
            client.call("routing_mode", {"mode": "concurrent", "multi_keyboard": True})

        baseline = fresh(both_released)
        for index, key in enumerate(("w", "d")):
            action("button", user=index, button="primary", pressed=True)
            action("key", user=index, key=key, pressed=True)
        held = fresh(both_held)
        require(all(r["mouse_clicks"] == b["mouse_clicks"]+1 and r["key_presses"] == b["key_presses"]+1
                    for r, b in zip(held, baseline)), "Native click/key handlers did not observe input")
        note("concurrent_distinct_key_and_button_delivery", held)

        if termination == "explicit-stop":
            moved_state = action("motion", user=0, dx=25, dy=18)
            moved = fresh(lambda rs: rs[0]["cursor"] == [held[0]["cursor"][0]+25, held[0]["cursor"][1]+18])
            require(moved[0]["drag_moves"] > held[0]["drag_moves"] and moved[1]["cursor"] == held[1]["cursor"], "Motion/drag delivery crossed targets")
            require(moved[1]["mouse_moves"] == held[1]["mouse_moves"], "Peer received motion event")
            note("native_motion_drag_no_crossover", {"reports": moved, "state": moved_state})
            action("wheel", user=0, delta=120)
            wheel_a = fresh(lambda rs: rs[0]["wheel_events"] > moved[0]["wheel_events"])
            require(wheel_a[1]["wheel_events"] == moved[1]["wheel_events"], "A wheel crossed to B")
            action("wheel", user=1, delta=-120)
            wheel_b = fresh(lambda rs: rs[1]["wheel_events"] > wheel_a[1]["wheel_events"])
            require(wheel_b[0]["wheel_events"] == wheel_a[0]["wheel_events"], "B wheel crossed to A")
            note("native_wheel_no_crossover", wheel_b)
            paused = action("pause")
            require(paused["paused"] and all(not u["buttons"] and not u["keys"] for u in paused["users"]), "Pause retained controller holds")
            paused_reports = fresh(both_released)
            note("pause_releases_native_holds", paused_reports)
            resumed = action("resume")
            require(not resumed["paused"], "Resume failed")
            for index, key in enumerate(("w", "d")):
                action("button", user=index, button="primary", pressed=True)
                action("key", user=index, key=key, pressed=True)
            note("native_holds_before_explicit_stop", fresh(both_held))

        state = client.call("routing_state")
        overlays = [u["overlay"]["hwnd"] for u in state["users"]]
        require(all(user32.IsWindow(w) for w in overlays), "Missing active virtual overlay")
        if termination == "explicit-stop":
            stopped = client.call("routing_stop")
            require(stopped["running"] is False and stopped["users"] == [] and stopped.get("last_error") is None, "Explicit shutdown not acknowledged")
            require(client.call("routing_stop") == stopped, "Stop not idempotent")
            note("explicit_stop_acknowledged", {"state": stopped, "scope": "EventStopped follows successful shutdown/fence on these ordinary probe pumps; per-recipient counts not exposed by MCP"})
        client.close()
        case["mcp_exit_code"] = client.process.returncode
        case["mcp_stderr"] = "".join(client.errors)
        client = None
        eventually(lambda: [bool(user32.IsWindow(w)) for w in overlays], lambda ws: not any(ws))
        released = fresh(both_released)
        note("post_shutdown_release_and_overlay_removal", {"reports": released, "overlays_exist": [bool(user32.IsWindow(w)) for w in overlays],
             "scope": "Post-unhook API zeros are supplemental; explicit-stop ACK is the fence oracle. EOF does not independently expose fence acknowledgement."})
        remaining = eventually(lambda: scoped_processes(scratch), lambda ps: {p["pid"] for p in ps} == set(allowed))
        note("only_owned_targets_remain_after_session", remaining)
        for image in (exe, fixture or exe):
            for hwnd, pid in gui.owned_windows(image):
                if pid in allowed: require(user32.PostMessageW(hwnd, 0x10, 0, 0), "Owned close failed")
        for probe in probes:
            probe.wait(timeout=15)
            require(probe.returncode == 0, "Probe failed graceful close")
        require(not scoped_processes(scratch), "Owned process survived normal cleanup")
        case["probe_exit_codes"] = [p.returncode for p in probes]
        case["result"] = "PASS"
    finally:
        if client:
            try: client.close()
            except Exception: pass
            (run/"mcp.stderr.txt").write_text("".join(client.errors), encoding="utf-8")
        elif "mcp_stderr" in case:
            (run/"mcp.stderr.txt").write_text(case["mcp_stderr"], encoding="utf-8")
        forced = scoped_processes(scratch, terminate=True)
        case["forced_cleanup"] = forced
        for probe in probes:
            if probe.poll() is None: probe.wait(timeout=10)
        for stream in streams: stream.close()
        case["remaining_owned_processes"] = scoped_processes(scratch)
        require(not case["remaining_owned_processes"], "Cleanup left owned processes")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="New evidence directory")
    parser.add_argument("--x86-probe", type=Path, help="External test fixture only; copied, never modified")
    parser.add_argument("--x86-sha256", help="Expected immutable external fixture SHA256")
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    report = {"result": "FAIL", "scope": "Virtual-only standalone MCP routing from unmodified portable ZIP; owned native probe targets",
              "archive_sha256": sha(args.archive), "phases": [], "hardware_or_rustdesk_input_used": False,
              "harness_sha256": {p.name: sha(p) for p in (Path(__file__), Path(__file__).with_name('test-portable.py'), Path(__file__).with_name('test-portable-gui.py'))}}
    scratch = None
    try:
        with tempfile.TemporaryDirectory(prefix="ProtoInputMultiPlex portable routing ") as directory:
            scratch = Path(directory).resolve()
            report["disposable_root"] = str(scratch)
            with zipfile.ZipFile(args.archive) as archive:
                require(all(not Path(i.filename).is_absolute() and ".." not in Path(i.filename).parts for i in archive.infolist()), "Unsafe archive member")
                archive.extractall(scratch)
            bundle = scratch/"ProtoInputMultiPlex"
            base.verify_manifest(bundle)
            manifest = json.loads((bundle/"bundle-manifest.json").read_text(encoding="utf-8"))
            report["manifest_sha256"] = sha(bundle/"bundle-manifest.json")
            report["application_sha256"] = sha(bundle/"protoinputmultiplex.exe")
            report["payload_hashes_before"] = {i["path"]: i["sha256"] for i in manifest["files"]}
            unrelated = scratch/"unrelated working directory"
            unrelated.mkdir()
            env = dict(os.environ)
            system = Path(os.environ.get("SystemRoot", r"C:\Windows"))
            env["PATH"] = os.pathsep.join((str(system/"System32"), str(system)))
            env["LOCALAPPDATA"] = str(scratch/"disposable LocalAppData")
            fixture = None
            if args.x86_probe:
                fixture = scratch/"fixtures/owned x86 probe.exe"
                fixture.parent.mkdir()
                shutil.copyfile(args.x86_probe, fixture)
                require(not args.x86_sha256 or sha(fixture) == args.x86_sha256.lower(), "External fixture SHA256 changed before copy")
                data = fixture.read_bytes()
                pe = int.from_bytes(data[0x3c:0x40], "little")
                require(data[pe:pe+4] == b"PE\0\0" and int.from_bytes(data[pe+4:pe+6], "little") == 0x14c, "External fixture is not x86 PE")
                report["external_x86_fixture"] = {"source": str(args.x86_probe.resolve()), "sha256": sha(fixture), "scope": "Disposable target only; never host/runtime or bundle payload"}
            # Each phase creates fresh PIDs; never reattach stopped/tainted targets.
            phase(bundle, fixture, scratch, env, unrelated, output, "explicit-stop", report)
            phase(bundle, fixture, scratch, env, unrelated, output, "eof-with-held-input", report)
            base.verify_manifest(bundle)
            report["payload_hashes_after_match"] = True
            report["remaining_owned_processes"] = scoped_processes(scratch)
            require(not report["remaining_owned_processes"], "Owned processes survived test")
        report["disposable_root_removed"] = not scratch.exists()
        report["result"] = "PASS"
    except Exception:
        report["error"] = traceback.format_exc()
        report["disposable_root_removed"] = scratch is not None and not scratch.exists()
    (output/"verification.json").write_text(json.dumps(report, indent=2)+"\n", encoding="utf-8")
    print(json.dumps({"result": report["result"], "evidence": str(output)}), flush=True)
    return 0 if report["result"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
