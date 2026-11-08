"""Bounded native GUI and connected-agent acceptance for annotation PNG export.

The module has no native imports or app launches before the explicit READY
gate. This is a single-session acceptance workflow; every external mutation is
claimed before it is attempted and is never automatically replayed.
"""
import argparse
import hashlib
import importlib.util
import json
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def digest(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def workflow(args):
    # Keep all Windows, UI, MCP and image imports behind --ready.
    import ctypes
    from ctypes import wintypes
    import copy
    import os
    import tempfile

    lifecycle = module("annotation_export_lifecycle", "test-app-lifecycle-ui.py")
    freeze_module = module("annotation_export_freeze", "test-shared-emergency-stop-native.py")
    connected = module("annotation_export_connected", "test-connected-agents-acceptance.py")
    geometry = module("annotation_export_geometry", "owned-gui-geometry.py")
    geometry.physical_pixel_context()
    Gate, ui, _, _ = lifecycle.gate_type()
    ui.user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
    ui.user32.GetClientRect.argtypes = [wintypes.HWND, ctypes.POINTER(geometry.Rect)]
    ui.user32.ClientToScreen.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.POINT)]
    ui.user32.ScreenToClient.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.POINT)]
    ui.user32.SetWindowPos.argtypes = [wintypes.HWND, wintypes.HWND, ctypes.c_int, ctypes.c_int,
        ctypes.c_int, ctypes.c_int, wintypes.UINT]
    ui.user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, ctypes.c_size_t, ctypes.c_ssize_t]
    producer = freeze_module.freeze(args)
    reused_capture_denial = None
    if args.resume_prefix:
        prefix = args.resume_prefix.resolve()
        prefix.relative_to(args.runtime.resolve())
        raw = prefix / "evidence.json"
        assert digest(raw) == "f8bf8592b71109fa88e10b495877193b3f4aab34ce317e10443dc84fb3d2fc17"
        previous = json.loads(raw.read_text(encoding="utf-8"))
        assert previous["result"] == "NOT ACCEPTED"
        denied = previous["imperative_mutations"]["export:deny-capture-off"]
        assert denied["state"] == "complete" and denied["result"]["isError"]
        assert "PNG export is disabled" in json.dumps(denied["result"])
        reused_capture_denial = {"path": str(raw), "sha256": digest(raw),
            "subgroup": "capture-off denial only; original overall verdict unchanged"}
    required_hashes = {digest(args.executable), digest(args.x86_executable)}
    producer_hashes = set(producer["build_sha256"].values())
    assert required_hashes <= producer_hashes, "Producer freeze does not bind both supplied executable images"
    reused_local_gui = None
    if args.resume_gui_prefix:
        prefix = args.resume_gui_prefix.resolve()
        prefix.relative_to(args.runtime.resolve())
        raw = prefix / "evidence.json"
        review = prefix / "independent-prefix-review.json"
        assert digest(raw) == "f4a35c88c7b634346604d71db3149cfd1a1e11baca3e7791fd61f2083f49f038"
        assert digest(review) == "a7c82d5df5f0c324eead51935307e15f74f75787dcb4505e630de86d0fb96a7e"
        previous = json.loads(raw.read_text(encoding="utf-8"))
        assert previous["result"] == "NOT ACCEPTED"
        assert previous["producer_report_sha256"] == args.producer_sha256
        for name, png_key, info_key, scope in (
            ("gui-save-all", "default_gui_png", "default_gui_export", "all_users"),
            ("gui-save-selected", "selected_gui_png", "selected_gui_export", "selected_user"),
        ):
            sample = previous["samples"][name]
            assert sample[info_key]["ink_scope"] == scope and sample[info_key]["revision"] == 8
            for item in (sample[png_key], sample["fresh_frame"]):
                path = Path(item["path"]).resolve()
                path.relative_to(prefix)
                assert digest(path) == item["sha256"]
        reused_local_gui = {"path": str(raw), "sha256": digest(raw),
            "review_sha256": digest(review), "subgroup": "Both local PNG scopes; 540 whole Save/scope/kind/path; 420 visible Draw/clear/scope only. No compact Save or completion-copy pixel claim."}

    reused_connected = None
    if args.resume_connected_prefix:
        assert reused_local_gui and reused_capture_denial
        prefix = args.resume_connected_prefix.resolve()
        prefix.relative_to(args.runtime.resolve())
        raw = prefix / "evidence.json"
        assert digest(raw) == "b2d6f297a0e4edc3484c2a77ae9935f34fbf54eaa7e428ec2e26f2af23b8e6cf"
        review = prefix / "independent-prefix-review.json"
        assert digest(review) == "5432bee93c3ca125bb4ec8bc160fbe209d224ad444e555629eeac26e5b20f5b5"
        previous = json.loads(raw.read_text(encoding="utf-8"))
        assert previous["result"] == "NOT ACCEPTED"
        assert previous["producer_report_sha256"] == args.producer_sha256
        assert previous["cleanup_errors"] == [] and previous["owned_processes_closed"]
        sample = previous["samples"]["capture-both"]
        for name, metadata, pixels in (
            ("selected", sample["connected_selected_png"]["export"], sample["connected_selected_png"]["pixels"]),
            ("composition", sample["composition_png"]["export"]["annotation"]["export"], sample["composition_png"]["pixels"]),
            ("magnifier", sample["magnifier_regression"]["metadata"], sample["magnifier_regression"]["pixels"]),
        ):
            path = Path(metadata["path"]).resolve()
            path.relative_to(prefix)
            assert digest(path) == pixels["sha256"], name
        reused_connected = {"path": str(raw), "sha256": digest(raw),
            "review_sha256": digest(review),
            "subgroup": "User/PID denials, connected selected PNG, owned-client composition with peer native F5 and magnifier regression only; revocation remains pending."}

    class WorkflowGate(Gate):
        def __init__(self):
            self.action_lock = threading.RLock()
            super().__init__(args, "annotation-export-workflow", offline=False, count=0)
            self.mcp_clients = []
            self.probes_owned = []
            self.probe_reports = []
            self.mutations = {}
            self.input_count = 0
            self.export_count = 0
            self.descriptor = None
            self.frames = {}
            self.instance = {}
            self.destination = None
            self.async_call = None
            self.delay_hwnd = None
            self.delay_pid = None
            self.delay_value = 0
            self.gui_generation = None
            self._process_handles = []
            self.known_overlay_hwnds = set()

        def save(self):
            with self.action_lock:
                super().save()

        def once(self, name, operation):
            with self.action_lock:
                assert name not in self.mutations, f"Imperative action already attempted: {name}"
                row = {"state": "started", "claimed_at": time.time()}
                self.mutations[name] = row
                self.save()
            try:
                result = operation()
                with self.action_lock:
                    row.update(state="complete", result=result, completed_at=time.time())
                    self.save()
                    return result
            except BaseException as error:
                with self.action_lock:
                    row.update(state="failed", error=repr(error), completed_at=time.time())
                    self.save()
                raise

        def own_process_identity(self, process, role, retain=False):
            kernel = ctypes.WinDLL("kernel32", use_last_error=True)
            kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
            kernel.OpenProcess.restype = wintypes.HANDLE
            kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
            kernel.QueryFullProcessImageNameW.argtypes = [wintypes.HANDLE, wintypes.DWORD,
                wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD)]
            kernel.CloseHandle.argtypes = [wintypes.HANDLE]
            query = 0x1000 | 0x0400
            handle = kernel.OpenProcess(query, False, process.pid)
            assert handle, f"Could not open exact owned {role} process {process.pid}"
            if retain:
                self._process_handles.append(handle)
            creation = (wintypes.FILETIME(), wintypes.FILETIME(), wintypes.FILETIME(), wintypes.FILETIME())
            assert kernel.GetProcessTimes(handle, *(ctypes.byref(part) for part in creation))
            image = ctypes.create_unicode_buffer(32768)
            size = wintypes.DWORD(len(image))
            assert kernel.QueryFullProcessImageNameW(handle, 0, image, ctypes.byref(size))
            expected = Path(process.args[0]).resolve()
            actual = Path(image.value).resolve()
            assert actual.samefile(expected), (role, process.pid, actual, expected)
            cim = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-Command",
                f"Get-CimInstance Win32_Process -Filter 'ProcessId = {int(process.pid)}' | "
                "Select-Object ProcessId,ParentProcessId,CreationDate,ExecutablePath | ConvertTo-Json -Compress"],
                capture_output=True, text=True, timeout=12, check=True)
            row = json.loads(cim.stdout)
            assert int(row["ProcessId"]) == process.pid and Path(row["ExecutablePath"]).resolve().samefile(expected)
            ticks = (creation[0].dwHighDateTime << 32) | creation[0].dwLowDateTime
            identity = {"role": role.removesuffix("-check"), "pid": process.pid,
                "image": str(actual), "creation_filetime": ticks,
                "cim": {"parent_pid": row.get("ParentProcessId"), "creation_date": row.get("CreationDate"),
                    "image": row.get("ExecutablePath")}}
            if not role.endswith("-check"):
                self.instance[role] = identity
            if not retain:
                kernel.CloseHandle(handle)
            return identity

        def check_process(self, role, process):
            saved = self.instance[role]
            assert process.poll() is None, f"Owned {role} process exited"
            current = self.own_process_identity(process, role + "-check")
            assert current["pid"] == saved["pid"] and current["creation_filetime"] == saved["creation_filetime"]
            assert Path(current["image"]).samefile(saved["image"])
            return saved

        def main_hwnd(self):
            hwnd = super().main_hwnd()
            if hwnd:
                pid = wintypes.DWORD()
                thread = ui.user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
                assert pid.value == self.gui.pid and thread
                self.gui_generation = {"hwnd": int(hwnd), "pid": pid.value,
                    "process": self.instance.get("gui"), "thread": thread}
            return hwnd

        def snapshot(self):
            return ui.read_json(self.run / "ui.profile.json")

        def open_mcp(self, label):
            files = list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{self.gui.pid}-*.json"))
            assert len(files) == 1, f"Expected one descriptor for exact GUI PID; got {len(files)}"
            descriptor = files[0]
            client = connected.ConnectedMcp(self.images[0], descriptor, self.run / f"{label}.stderr.log")
            self.mcp_clients.append(client)
            self.descriptor = descriptor
            return client

        def reconnect(self, old_clients):
            for client in old_clients:
                if client in self.mcp_clients:
                    client.close()
                    self.mcp_clients.remove(client)
            return self.open_mcp("connected-current-" + str(len(self.mutations)))

        def state(self, client):
            return client.tool("annotation", {"operation": "inspect"})["annotation"]

        def export(self, client, label, user, scope="all_users", kind="ink_only", path=None, error=False):
            self.export_count += 1
            assert self.export_count <= 12, "Export mutation cap exceeded"
            destination = path or (self.run / f"{label}.png")
            cmd = {"operation": "export", "user": user, "kind": kind, "path": str(destination)}
            if scope is not None:
                cmd["ink_scope"] = scope
            def run():
                return client.tool("annotation", cmd, error=error)
            result = self.once("export:" + label, run)
            return destination, result

        def input(self, client, label, action, **fields):
            self.input_count += 1
            assert self.input_count <= 30, "routing_input mutation cap exceeded"
            return self.once("routing_input:" + label,
                lambda: client.tool("routing_input", {"action": action, **fields}))

        def probes(self):
            return [ui.read_json(path) for path in self.probe_reports]

        def report(self, index):
            return ui.read_json(self.probe_reports[index])

        def wait_pending(self, phase):
            path = self.run / "ui.annotation.json"
            def read():
                value = ui.read_json(path)
                pending = value.get("export_pending")
                return value if pending and pending.get("phase") == phase else None
            return ui.eventually(read, bool, 20)

        def no_output(self, destination):
            assert not destination.exists(), f"Denied export created {destination.name}"
            assert not list(destination.parent.glob(destination.name + ".mux-export.*.tmp")), "Denied export left a temporary file"

        def check_png(self, path, expected_size, pixels, mode="RGBA"):
            from PIL import Image
            assert path.is_file() and path.stat().st_size > 64, path
            with Image.open(path) as image:
                image.load()
                assert image.mode == mode, (path.name, image.mode)
                assert list(image.size) == list(expected_size), (path.name, image.size, expected_size)
                observed = {}
                for name, (point, predicate) in pixels.items():
                    color = tuple(image.getpixel(tuple(point)))
                    assert predicate(color), (path.name, name, point, color)
                    observed[name] = color
                return {"size": list(image.size), "mode": image.mode, "pixels": observed,
                    "sha256": digest(path), "bytes": path.stat().st_size}

        def set_delay(self, index, milliseconds):
            report = self.report(index)
            hwnd = int(report["capture_hwnd"])
            pid = int(report["pid"])
            assert pid == self.probes_owned[index].pid
            assert hwnd == capture_rects[index]["hwnd"]
            assert int(geometry.owned_window(pid)) == int(target_windows[index]["hwnd"])
            assert ui.user32.IsWindow(hwnd)
            owner = wintypes.DWORD()
            ui.user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
            assert owner.value == pid
            saved = self.instance[f"probe-{index}"]
            self.check_process(f"probe-{index}", self.probes_owned[index])
            assert saved["pid"] == owner.value
            def post_delay():
                assert ui.user32.PostMessageW(hwnd, 0x8052, milliseconds, 0)
                report_path = self.probe_reports[index]
                return ui.eventually(lambda: ui.read_json(report_path),
                    lambda value: value.get("capture_delay_ms") == milliseconds, 8)
            self.once(f"capture-delay:{index}:{milliseconds}", post_delay)
            self.delay_hwnd, self.delay_pid, self.delay_value = hwnd, pid, milliseconds
            return {"hwnd": hwnd, "pid": pid, "delay_ms": milliseconds,
                "process_creation_filetime": saved["creation_filetime"]}

        def clear_delay(self):
            if self.delay_value:
                assert ui.user32.IsWindow(self.delay_hwnd)
                owner = wintypes.DWORD()
                ui.user32.GetWindowThreadProcessId(self.delay_hwnd, ctypes.byref(owner))
                assert owner.value == self.delay_pid
                def clear():
                    assert ui.user32.PostMessageW(self.delay_hwnd, 0x8052, 0, 0)
                    return ui.eventually(lambda: self.report(0), lambda value: value.get("capture_delay_ms") == 0, 8)
                self.once(f"capture-delay:0:clear:{self.delay_value}", clear)
                self.delay_value = 0

        def composition_thread(self, client, label, user, scope, destination):
            assert self.async_call is None
            slot = {"result": None, "error": None, "done": threading.Event(), "path": destination}
            def worker():
                try:
                    _, slot["result"] = self.export(client, label, user, scope, "selected_target", destination)
                except BaseException as error:
                    slot["error"] = repr(error)
                finally:
                    slot["done"].set()
            self.async_call = slot
            self.once("start-worker:" + label, lambda: (threading.Thread(target=worker, name=label, daemon=True).start(),
                {"worker_started": True})[1])
            return slot

        def frame(self, label, size):
            if label in self.frames:
                return self.frames[label]
            hwnd = self.main_hwnd()
            assert hwnd
            rect = self.once("resize-ui-frame:" + label,
                lambda: (geometry.resize_owned_client(hwnd, size), geometry.geometry(hwnd))[1])
            assert all(abs(a-b) < 1 for a, b in zip(rect["client_logical"], size)), rect
            self.frames[label] = {"capture_started": time.time(), "hwnd": int(hwnd), "size": list(size), "geometry": rect}
            return self.frames[label]

        def capture_frame(self, label):
            frame = self.frames[label]
            destination = self.run / (label + ".png")
            copied = geometry.copy_owned_product_frame(self.run / "ui.png", destination, frame["hwnd"], frame["capture_started"])
            if not copied:
                return None
            frame.update({"path": str(destination), "sha256": digest(destination), "bytes": destination.stat().st_size})
            return frame

        def close(self):
            cleanup_errors = []
            try:
                self.clear_delay()
            except Exception as error:
                cleanup_errors.append("delay: " + repr(error))
            for client in list(self.mcp_clients):
                try:
                    client.close()
                    self.mcp_clients.remove(client)
                except Exception as error:
                    cleanup_errors.append("mcp: " + repr(error))
            if self.gui and self.gui.poll() is None:
                try:
                    hwnd = self.main_hwnd()
                    if hwnd:
                        self.once("failure-close-owned-gui", lambda: ui.user32.PostMessageW(hwnd, 0x0010, 0, 0))
                        self.gui.wait(timeout=12)
                except Exception as error:
                    self.evidence["failure_graceful_close_error"] = repr(error)
            for proc in reversed(self.owned):
                if proc.poll() is None:
                    try:
                        self.once(f"cleanup-terminate-owned-{proc.pid}", proc.terminate)
                        proc.wait(timeout=10)
                    except Exception as error:
                        cleanup_errors.append(f"process {proc.pid}: {error!r}")
            try:
                if "remove-startup-test-key" not in self.mutations:
                    self.once("remove-startup-test-key", lambda: lifecycle.remove_startup_key(self.startup_key))
                assert lifecycle.startup_value(self.startup_key) is None
                descriptors = list(Path(tempfile.gettempdir()).glob(
                    f"protoinputmultiplex-agent-{self.gui.pid}-*.json")) if self.gui else []
                assert not descriptors, "Owned GUI descriptor remains"
                live_overlays = [hwnd for hwnd in self.known_overlay_hwnds if ui.user32.IsWindow(hwnd)]
                assert not live_overlays, f"Known overlay windows remain: {live_overlays}"
                self.evidence["final_resource_audit"] = {"startup_key_absent": True,
                    "descriptor_absent": True, "known_overlays_closed": sorted(self.known_overlay_hwnds)}
            except Exception as error:
                cleanup_errors.append("resource audit: " + repr(error))
            kernel = ctypes.WinDLL("kernel32", use_last_error=True)
            kernel.CloseHandle.argtypes = [wintypes.HANDLE]
            for handle in self._process_handles:
                kernel.CloseHandle(handle)
            self._process_handles.clear()
            self.evidence["cleanup_errors"] = cleanup_errors
            self.evidence["owned_processes_closed"] = all(proc.poll() is not None for proc in self.owned)
            self.evidence["imperative_mutations"] = self.mutations
            self.evidence["routing_input_count"] = self.input_count
            self.evidence["annotation_export_count"] = self.export_count
            image_literals = ["'" + str(Path(path).resolve()).replace("'", "''") + "'"
                for path in self.images]
            predicate = " -or ".join("$_.ExecutablePath -eq " + path for path in image_literals)
            try:
                audit = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-Command",
                    "@(Get-CimInstance Win32_Process | Where-Object { " + predicate +
                    " } | Select-Object ProcessId,ParentProcessId,CreationDate,ExecutablePath) | ConvertTo-Json -Compress"],
                    capture_output=True, text=True, timeout=20)
                assert audit.returncode == 0, audit.stderr
                remaining = json.loads(audit.stdout) if audit.stdout.strip() else []
                if isinstance(remaining, dict):
                    remaining = [remaining]
                self.evidence["owned_image_process_audit"] = remaining
                assert not remaining, "Owned run-image process remains after cleanup"
            except Exception as error:
                cleanup_errors.append("process audit: " + repr(error))
            if cleanup_errors or not self.evidence["owned_processes_closed"]:
                self.evidence["result"] = "NOT ACCEPTED"
            self.save()
            assert not cleanup_errors and self.evidence["owned_processes_closed"], cleanup_errors

    gate = WorkflowGate()
    assert digest(gate.images[0]) == digest(args.executable), "Lifecycle x64 image copy changed after producer freeze"
    assert digest(gate.images[1]) == digest(args.x86_executable), "Lifecycle x86 image copy changed after producer freeze"
    step = ui.step
    run = gate.run
    evidence = gate.evidence
    evidence.update({"result": "NOT ACCEPTED", "producer_sha256": args.producer_sha256,
        "producer_report_sha256": digest(args.producer), "source_hashes": {},
        "bound_executable_sha256": {str(args.executable): digest(args.executable),
            str(args.x86_executable): digest(args.x86_executable)},
        "reused_capture_off_denial": reused_capture_denial,
        "reused_local_gui_exports": reused_local_gui,
        "reused_connected_exports": reused_connected,
        "fixture_note": "Probe CWD follows accepted root-CWD fixtures; physical geometry context is explicit. Quiet retry does not establish why the earlier lifecycle open failed.",
        "scope": "one virtual GUI session; two owned x64 capture-pattern clients; connected and GUI ink scopes"})
    provenance = run / "runner-source"
    provenance.mkdir(exist_ok=True)
    for filename in (Path(__file__).name, "test-app-lifecycle-ui.py", "test-profile-maintenance.py",
        "test-connected-agents-acceptance.py", "test-mcp.py", "test-shared-emergency-stop-native.py",
        "owned-gui-geometry.py", "test-annotation-acceptance.py", "test-magnifier-acceptance.py"):
        source = Path(__file__).with_name(filename) if filename != Path(__file__).name else Path(__file__)
        shutil.copy2(source, provenance / filename)
        evidence["source_hashes"][filename] = digest(source)
    dependency_hashes = {path.name: digest(path) for path in Path(__file__).parent.glob("*.py")}
    ui.put(provenance / "all-python-dependency-hashes.json", dependency_hashes)
    ui.put(provenance / "producer-binding.json", {"producer": str(args.producer.resolve()),
        "producer_sha256": digest(args.producer), "requested_sha256": args.producer_sha256,
        "executables": evidence["bound_executable_sha256"]})
    shutil.copy2(args.producer, provenance / "producer-verification.json")

    colors = ([230, 40, 70, 255], [40, 100, 220, 255])
    annotation = {"tool": "brush", "activator": "middle", "color": colors[0][:3], "alpha": 255,
        "width": 8, "clear_key": None, "clear_all": False}
    seats = [{"id": f"anno-{letter}", "name": f"Annotation {letter.upper()}",
        "mouse": f"virtual://mouse/{i+1}", "keyboard": f"virtual://keyboard/{i+1}",
        "target_pid": 0, "annotation": {**annotation, "color": colors[i][:3]},
        "magnifier": {"mode": "lens" if i == 1 else "off", "source": "selected_target",
            "zoom": 2.0, "width": 64, "height": 64, "border": False,
            "border_color": [96, 165, 250], "auto_refresh": False, "refresh_hz": 8,
            "monitor_id": None, "work_area": True, "dock": "right", "desktop_region": None}}
        for i, letter in enumerate(("a", "b"))]
    profile = {"version": 1, "concurrent": True,
        "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": []}, "seats": seats}
    preferences = lifecycle.preference_defaults()
    preferences.update(launch_minimized=False, minimize_to_tray=False, close_to_tray=False, launch_at_login=False)
    preferences["shortcuts"] = []
    steps = [
        step("start", "Start routing"),
        step("settings", "Settings"), step("agent-category", "Settings category Agent access"),
        step("agent-page", "Agent Access"),
        step("agent-off", "Enable agent access", {"agent_users": "anno-a,anno-b", "agent_pids": "", "agent_capture": "false", "agent_session_control": "false"}),
        step("home", "Back to Home"), step("configure-a", "Configure user"), step("draw-a", "Draw"),
        step("gui-save-all", "Save annotation PNG", {"annotation_export_path": run / "gui-all.png"}),
        step("gui-selected-scope", "Export selected user ink"),
        step("gui-save-selected", "Save annotation PNG", {"annotation_export_path": run / "gui-selected.png"}),
        step("gui-restore-all", "Export all users ink"),
        step("home-agents", "Back to Home"), step("settings-agents", "Settings"),
        step("agent-category-reopen", "Settings category Agent access"), step("agent-page-reopen", "Agent Access"),
        step("capture-a", "Apply access policy", {"agent_users": "anno-a", "agent_pids": "", "agent_capture": "true", "agent_session_control": "false"}),
        step("capture-a-pid-b", "Apply access policy", {"agent_users": "anno-a", "agent_pids": "", "agent_capture": "true", "agent_session_control": "false"}),
        step("capture-both", "Apply access policy", {"agent_users": "anno-a,anno-b", "agent_pids": "", "agent_capture": "true", "agent_session_control": "false"}),
        step("capture-revoked", "Apply access policy", {"agent_users": "anno-a", "agent_pids": "", "agent_capture": "false", "agent_session_control": "false"}),
        step("home-stop", "Back to Home"), step("stop", "Stop routing")]
    if reused_local_gui:
        completed_local = {"home", "configure-a", "draw-a", "gui-save-all", "gui-selected-scope",
            "gui-save-selected", "gui-restore-all", "home-agents", "settings-agents",
            "agent-category-reopen", "agent-page-reopen"}
        steps = [row for row in steps if row["id"] not in completed_local]
    if reused_connected:
        steps = [row for row in steps if row["id"] not in {"capture-a", "capture-a-pid-b"}]
    # Script exposes the next step's fields while awaiting the current sample.
    # A field-free disabled observation fences each policy without another Apply.
    fenced = []
    for row in steps:
        fenced.append(row)
        if row["id"] in {"agent-off", "capture-a", "capture-a-pid-b", "capture-both"}:
            fenced.append(step(row["id"] + "-sample-fence", "Enable agent access", disabled=True))
    steps = fenced

    def actual_settings(sample, users, pids, capture):
        # Each requested policy is distinct. Wait for its committed GUI ACK,
        # not a draft or an older nonpending snapshot, before reconnecting.
        def acknowledged():
            path = run / "ui.agent.acks.jsonl"
            if not path.exists():
                return None
            rows = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
            return rows[-1] if rows else None
        def matches(value):
            return (value is not None and value.get("pending") is False
                and value.get("policy_enabled") is True and value.get("error") is None
                and sorted(value.get("selected_users", [])) == sorted(users)
                and sorted(value.get("allowed_pids", [])) == sorted(pids)
                and value.get("capture") is capture
                and value.get("session_control") is False
                and value.get("app_preferences") is False)
        snap = ui.eventually(acknowledged, matches, 12)
        sample["acknowledged_policy"] = snap
        return snap

    def committed_policy(client, users, pids, capture):
        def matches(session):
            policy = session.get("policy") or {}
            return (session.get("connected") is True and int(session.get("client") or 0) > 0
                and policy.get("enabled") is True
                and sorted(policy.get("user_ids", [])) == sorted(users)
                and sorted(policy.get("allowed_pids", [])) == sorted(pids)
                and policy.get("capture") is capture
                and policy.get("session_control") is False
                and policy.get("app_preferences") is False)
        session = ui.eventually(lambda: client.tool("routing_state")["agent_session"], matches, 12)
        return {"client": session["client"], "policy": session["policy"]}

    def gui_export_info(path):
        def completed():
            acknowledgements = [json.loads(line) for line in
                (run / "ui.annotation.acks.jsonl").read_text(encoding="utf-8").splitlines()]
            matches = [state for state in acknowledgements if state.get("export")
                and state["export"].get("path") == str(path) and state.get("error") is None]
            current = ui.read_json(run / "ui.annotation.json")
            return matches[-1] if matches and current.get("export_pending") is None else None
        return ui.eventually(completed, bool, 15)

    probes_ready = False
    gui_frame_started = False
    sentinel = run / "protected-sentinel.png"
    sentinel_bytes = b"annotation export protected destination\x00preserve"
    gate.once("create-owned-preservation-sentinel", lambda: (sentinel.write_bytes(sentinel_bytes), {"bytes": len(sentinel_bytes)})[1])
    sentinel_hash = digest(sentinel)
    clients = {}
    default_gui_exported = False
    selected_gui_exported = False
    delayed = False
    revoked_worker_started = False
    fresh_after_revoke = None
    gui_exited = False
    overlays = []
    display = None
    target_windows = []
    target_rects = []
    capture_rects = []
    A_POINT = None
    B_POINT = None

    def own_probe(index, rgb):
        report = run / f"capture-{index}.json"
        created = {}
        def spawn():
            process = subprocess.Popen([str(gate.images[0]), "probe", "--automation", "--capture-pattern",
                str((rgb[0] << 16) | (rgb[1] << 8) | rgb[2]), "--report", str(report)],
                cwd=ROOT, creationflags=subprocess.CREATE_NO_WINDOW)
            created["process"] = process
            return {"pid": process.pid, "report": str(report)}
        gate.once(f"spawn-pattern-probe-{index}", spawn)
        process = created["process"]
        gate.owned.append(process)
        gate.probes_owned.append(process)
        gate.probe_reports.append(report)
        gate.own_process_identity(process, f"probe-{index}")
        initial = ui.eventually(lambda: ui.read_json(report), lambda value:
            value.get("frames", 0) > 3 and value.get("capture_hwnd") and value.get("pid") == process.pid, 15)
        return initial

    def connect(label="initial"):
        client = gate.open_mcp(label)
        client.rpc("tools/list", {})
        return client

    def route_draw(client, index, label):
        x, y = (A_POINT, B_POINT)[index]
        # Expose only the exact disposable target, without activation or OS input.
        # Initial placement retained z-order and could leave a test point occluded.
        gate.check_process(f"probe-{index}", gate.probes_owned[index])
        hwnd = int(target_windows[index]["hwnd"])
        assert int(geometry.owned_window(gate.probes_owned[index].pid)) == hwnd
        owner = wintypes.DWORD()
        ui.user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        assert owner.value == gate.probes_owned[index].pid
        def expose_owned_target():
            assert ui.user32.SetWindowPos(hwnd, wintypes.HWND(-1), 0, 0, 0, 0,
                0x0001 | 0x0002 | 0x0010)  # NOSIZE | NOMOVE | NOACTIVATE
            extended_style = int(geometry.user32.GetWindowLongPtrW(hwnd, -20))
            capture = capture_rects[index]
            child_owner = wintypes.DWORD()
            ui.user32.GetWindowThreadProcessId(capture["hwnd"], ctypes.byref(child_owner))
            assert child_owner.value == owner.value
            origin, bounds = wintypes.POINT(0, 0), geometry.Rect()
            assert ui.user32.ClientToScreen(capture["hwnd"], ctypes.byref(origin))
            assert ui.user32.GetClientRect(capture["hwnd"], ctypes.byref(bounds))
            actual = {"left": origin.x, "top": origin.y,
                "right": origin.x + bounds.right, "bottom": origin.y + bounds.bottom}
            assert all(actual[key] == capture[key] for key in actual)
            assert actual["left"] <= x < actual["right"] and actual["top"] <= y < actual["bottom"]
            return {"pid": owner.value, "root_hwnd": hwnd, "capture_hwnd": capture["hwnd"],
                "capture_bounds": actual, "extended_style": extended_style,
                "topmost_observed": bool(extended_style & 0x00000008), "activation_requested": False}
        gate.once(f"expose-owned-probe-{index}", expose_owned_target)
        gate.input(client, label + "-select-move", "move_to", user=index, x=x, y=y)
        selected = gate.input(client, label + "-select-down", "button", user=index, button="primary", pressed=True)
        route = selected["users"][index]
        assert route["pid"] == gate.probes_owned[index].pid and route["status"].startswith("Concurrent"), route["status"]
        gate.input(client, label + "-select-up", "button", user=index, button="primary", pressed=False)
        ui.eventually(lambda: gate.report(index), lambda report:
            [event["pressed"] for event in report["mouse_events"] if event["button"] == 0] == [True, False], 6)
        gate.input(client, label + "-middle-down", "button", user=index, button="middle", pressed=True)
        gate.input(client, label + "-stroke-move", "move_to", user=index, x=x + 46, y=y)
        gate.input(client, label + "-middle-up", "button", user=index, button="middle", pressed=False)
        return {"user": index, "target_point": [x, y]}

    def draw_evidence(client):
        state = client.tool("routing_state")
        users = state.get("users", [])
        assert len(users) == 2
        result = []
        annotation_state = gate.state(client)
        if annotation_state.get("overlay"):
            gate.known_overlay_hwnds.add(int(annotation_state["overlay"]["hwnd"]))
        for i, (user, proc) in enumerate(zip(users, gate.probes_owned)):
            assert int(user["pid"]) == proc.pid
            report = gate.report(i)
            assert int(report["capture_hwnd"]) == capture_rects[i]["hwnd"]
            primary = [event for event in report.get("mouse_events", []) if event.get("button") == 0]
            assert len(primary) == 2 and [event["pressed"] for event in primary[-2:]] == [True, False]
            assert not report.get("buttons"), "Probe input-release counter retained a held button"
            result.append({"user": i, "pid": proc.pid, "window": target_windows[i],
                "native_primary_edges": primary[-2:], "native_buttons_after_release": report.get("buttons", 0),
                "router_user": user})
        return result

    def no_effect_denial(client, name, user, scope, kind="ink_only", sentinel_path=None):
        destination = run / (name + ".png")
        before = gate.state(client)
        if sentinel_path:
            destination = sentinel_path
        _, reply = gate.export(client, name, user, scope, kind, destination, error=True)
        gate.no_output(destination) if not sentinel_path else None
        if sentinel_path:
            assert destination.read_bytes() == sentinel_bytes and digest(destination) == sentinel_hash
        after = gate.state(client)
        def stable(value):
            return {"revision": value["revision"], "users": [{key: row.get(key) for key in
                ("user", "settings", "strokes", "segments", "open")} for row in value["users"]]}
        before_stable, after_stable = stable(before), stable(after)
        assert after_stable == before_stable, f"Denied {name} changed ink or revision"
        reply_text = json.dumps(reply, ensure_ascii=False).lower()
        reason = ("png export is disabled" if name == "deny-capture-off" else
            "unknown user index" if name == "deny-b-selected" else
            "this user is not permitted by agent access")
        assert reason in reply_text, reply_text
        return {"destination_absent_or_preserved": True, "ink_and_revision_unchanged": True,
            "error": reply,
                "before_revision": before["revision"], "after_revision": after["revision"]}

    def begin_async(client, label, scope, delay):
        destination = run / (label + ".png")
        gate.set_delay(0, delay)
        slot = gate.composition_thread(client, label, 0, scope, destination)
        gate.wait_pending("preparing")
        return slot

    def verify(name, sample):
        nonlocal probes_ready, default_gui_exported, selected_gui_exported, delayed
        nonlocal revoked_worker_started, fresh_after_revoke, gui_exited, overlays
        if name == "start":
            ready = ui.eventually(lambda: ui.read_json(run / "ui.state.json"),
                lambda rows: len(rows) == 2 and all(row.get("overlay")
                    and row["overlay"].get("hwnd") and ui.user32.IsWindow(row["overlay"]["hwnd"])
                    for row in rows) and len({row["overlay"]["hwnd"] for row in rows}) == 2, 10)
            overlays = [u["overlay"]["hwnd"] for u in ready]
            gate.known_overlay_hwnds.update(overlays)
            sample["owned_probes"] = [gate.report(i) for i in range(2)]
            sample["owned_geometry"] = {"desktop": display, "targets": target_windows,
                "points": [A_POINT, B_POINT], "profile_pids": profile["desktop"]["allowed_pids"]}
            probes_ready = True
        if name == "agent-off":
            snap = actual_settings(sample, ["anno-a", "anno-b"],
                [proc.pid for proc in gate.probes_owned], False)
            assert snap["selected_users"] == ["anno-a", "anno-b"]
            assert set(snap["allowed_pids"]) == {proc.pid for proc in gate.probes_owned}
            assert not snap["capture"] and not snap["session_control"]
            if "initial" not in clients:
                clients["initial"] = connect()
                sample["committed_policy"] = committed_policy(clients["initial"],
                    ["anno-a", "anno-b"], [proc.pid for proc in gate.probes_owned], False)
                if reused_capture_denial:
                    sample["capture-off-denial-reused"] = reused_capture_denial
                else:
                    sample["capture-off-denial"] = no_effect_denial(clients["initial"],
                        "deny-capture-off", 0, "selected_user")
                sample["drawing"] = [route_draw(clients["initial"], i, f"draw-{i}") for i in range(2)]
                sample["selection_and_native_release"] = draw_evidence(clients["initial"])
        if name == "gui-save-all":
            path = run / "gui-all.png"
            gui_info = gui_export_info(path)
            assert path.exists()
            from PIL import Image
            with Image.open(path) as im:
                im.load()
                assert im.mode == "RGBA"
                assert list(im.size) == [display["right"] - display["left"], display["bottom"] - display["top"]]
                p0 = (A_POINT[0] - display["left"], A_POINT[1] - display["top"])
                p1 = (B_POINT[0] - display["left"], B_POINT[1] - display["top"])
                assert tuple(im.getpixel(p0))[:3] == tuple(colors[0][:3])
                assert tuple(im.getpixel(p1))[:3] == tuple(colors[1][:3])
                assert im.getpixel((2, 2))[3] == 0
            assert gui_info["export"]["ink_scope"] == "all_users"
            if not default_gui_exported:
                frame = gate.frame("gui-export-default-540x520", [540, 520])
                copied = gate.capture_frame("gui-export-default-540x520")
                if not copied:
                    return False
                default_gui_exported = True
                sample["default_gui_png"] = {"path": str(path), "sha256": digest(path), "size": [display["right"] - display["left"], display["bottom"] - display["top"]]}
                sample["default_gui_export"] = gui_info["export"]
                sample["fresh_frame"] = copied
        if name == "gui-save-selected":
            path = run / "gui-selected.png"
            gui_info = gui_export_info(path)
            assert path.exists()
            from PIL import Image
            with Image.open(path) as im:
                im.load()
                assert im.mode == "RGBA"
                p0 = (A_POINT[0] - display["left"], A_POINT[1] - display["top"])
                p1 = (B_POINT[0] - display["left"], B_POINT[1] - display["top"])
                assert tuple(im.getpixel(p0))[:3] == tuple(colors[0][:3])
                assert im.getpixel(p1)[3] == 0
            assert gui_info["export"]["ink_scope"] == "selected_user"
            frame = gate.frame("gui-export-minimum-420x520", [420, 520])
            copied = gate.capture_frame("gui-export-minimum-420x520")
            if not copied:
                return False
            # Restore usable width once the representative minimum frame is copied.
            gate.once("resize-ui-restore-default-width", lambda:
                geometry.resize_owned_client(gate.main_hwnd(), [540, 520]) or {"size": [540, 520]})
            selected_gui_exported = True
            sample["selected_gui_png"] = {"path": str(path), "sha256": digest(path)}
            sample["selected_gui_export"] = gui_info["export"]
            sample["fresh_frame"] = copied
        if name == "gui-restore-all":
            sample["restored_scope_control"] = "Export all users ink"
        if name == "capture-a":
            snap = actual_settings(sample, ["anno-a"],
                [proc.pid for proc in gate.probes_owned], True)
            assert snap["capture"] and snap["selected_users"] == ["anno-a"]
            assert set(snap["allowed_pids"]) == {proc.pid for proc in gate.probes_owned}
            policy_before = snap
            clients["a-only"] = gate.reconnect([clients["initial"]])
            sample["committed_policy"] = committed_policy(clients["a-only"],
                ["anno-a"], [proc.pid for proc in gate.probes_owned], True)
            sample["denied-b-selected"] = no_effect_denial(clients["a-only"], "deny-b-selected", 1, "selected_user")
            sample["denied-a-all-preserves"] = no_effect_denial(clients["a-only"], "deny-a-all", 0,
                "all_users", sentinel_path=sentinel)
            sample["denied-omitted-default-all"] = no_effect_denial(clients["a-only"], "deny-omitted-scope", 0, None)
            policy_after = actual_settings(sample, ["anno-a"],
                [proc.pid for proc in gate.probes_owned], True)
            policy_fields = ("selected_users", "allowed_pids", "capture", "session_control")
            assert {key: policy_after[key] for key in policy_fields} == {
                key: policy_before[key] for key in policy_fields
            }, "Denied exports changed Agent Access controls"
        if name == "capture-a-pid-b":
            snap = actual_settings(sample, ["anno-a"],
                [gate.probes_owned[1].pid], True)
            assert snap["capture"] and snap["selected_users"] == ["anno-a"]
            assert snap["allowed_pids"] == [gate.probes_owned[1].pid]
            clients["pid-b-only"] = gate.reconnect([clients["a-only"]])
            sample["committed_policy"] = committed_policy(clients["pid-b-only"],
                ["anno-a"], [gate.probes_owned[1].pid], True)
            destination, denial = gate.export(clients["pid-b-only"], "deny-target-outside-pid-allowlist", 0,
                "selected_user", "selected_target", error=True)
            gate.no_output(destination)
            assert "outside agent access process permissions" in json.dumps(denial).lower(), denial
            sample["target_pid_denied"] = {"path_absent": True, "agent_policy": snap, "error": denial}
        if name == "capture-both":
            snap = actual_settings(sample, ["anno-a", "anno-b"],
                [proc.pid for proc in gate.probes_owned], True)
            assert snap["selected_users"] == ["anno-a", "anno-b"] and snap["capture"]
            assert set(snap["allowed_pids"]) == {proc.pid for proc in gate.probes_owned}
            clients["full-a"] = gate.reconnect([clients["initial"] if reused_connected else clients["pid-b-only"]])
            clients["full-b"] = gate.open_mcp("connected-peer-b")
            client = clients["full-a"]
            sample["committed_policy"] = [committed_policy(peer, ["anno-a", "anno-b"],
                [proc.pid for proc in gate.probes_owned], True)
                for peer in (client, clients["full-b"])]
            if reused_connected:
                sample["reused_connected_exports"] = reused_connected
            else:
                revision_state = gate.state(client)
                destination, result = gate.export(client, "connected-a-selected", 0, "selected_user")
                ui.eventually(lambda: destination.exists(), bool, 15)
                dims = [display["right"] - display["left"], display["bottom"] - display["top"]]
                info = result["annotation"]["export"]
                assert info.get("user") in (0, "anno-a", "a")
                assert info.get("ink_scope") == "selected_user"
                assert int(info["revision"]) == int(revision_state["revision"])
                sample["connected_selected_png"] = {"export": info, "pixels": gate.check_png(destination, dims, {
                    "A red midpoint": ((A_POINT[0] - display["left"], A_POINT[1] - display["top"]), lambda c: c[:3] == tuple(colors[0][:3])),
                    "B absent": ((B_POINT[0] - display["left"], B_POINT[1] - display["top"]), lambda c: c[3] == 0),
                    "untouched alpha": ((2, 2), lambda c: c[3] == 0)})}
                slot = begin_async(client, "composition-all-users", "all_users", 1500)
                sample["pending_composition"] = ui.read_json(run / "ui.annotation.json").get("export_pending")
                before = gate.report(1).get("key_events", [])
                bclient = clients["full-b"]
                gate.input(bclient, "busy-peer-f5-down", "key", user=1, key="f5", pressed=True)
                gate.input(bclient, "busy-peer-f5-up", "key", user=1, key="f5", pressed=False)
                report = ui.eventually(lambda: gate.report(1), lambda value:
                    len([event for event in value.get("key_events", []) if event.get("vk") == 116]) >= 2, 8)
                assert not slot["done"].is_set(), "A's preparation ended before peer input completed"
                f5 = [event for event in report.get("key_events", []) if event.get("vk") == 116]
                assert [event["pressed"] for event in f5[len(before):]] == [True, False]
                native_f5 = [event for event in report.get("native_events", [])
                    if event.get("kind") == "key" and event.get("code") == 116]
                assert len(native_f5) >= 2 and [event["pressed"] for event in native_f5[-2:]] == [True, False]
                assert all(event["hwnd"] == gate.report(1)["capture_hwnd"] for event in native_f5[-2:])
                assert not report.get("held_keys") and report.get("pid") == gate.probes_owned[1].pid
                assert slot["done"].wait(30), "Composition export did not finish"
                assert slot["error"] is None, slot["error"]
                composition = ui.eventually(lambda: ui.read_json(run / "ui.annotation.json"),
                    lambda value: value.get("export_pending") is None, 8)
                destination = slot["path"]
                target = capture_rects[0]
                capture_hwnd = gate.report(0)["capture_hwnd"]
                owner = wintypes.DWORD()
                ui.user32.GetWindowThreadProcessId(capture_hwnd, ctypes.byref(owner))
                assert owner.value == gate.probes_owned[0].pid
                sample["composition_png"] = {"export": slot["result"], "pixels": gate.check_png(destination,
                    [target["right"] - target["left"], target["bottom"] - target["top"]], {
                        "A red midpoint": ((A_POINT[0] + 23 - target["left"], A_POINT[1] - target["top"]), lambda c: c[:3] == tuple(colors[0][:3])),
                        "capture background": ((24, 24), lambda c: c[:3] == (70, 60, 90))}),
                    "peer_f5_events": report.get("key_events", []), "peer_held": report.get("held_keys", [])}
                composition_info = slot["result"]["annotation"]["export"]
                assert int(composition_info["pid"]) == gate.probes_owned[0].pid
                assert int(composition_info["hwnd"]) == int(capture_hwnd)
                assert int(composition_info["width"]) == target["right"] - target["left"]
                assert int(composition_info["height"]) == target["bottom"] - target["top"]
                assert composition_info.get("user") in (0, "anno-a", "a")
                assert composition_info.get("ink_scope") == "all_users"
                gate.clear_delay()
                gate.async_call = None
                magpath = run / "magnifier-b.png"
                mag = gate.once("magnifier-export-b", lambda: bclient.tool("magnifier", {
                    "operation": "export", "user": 1, "path": str(magpath)}))
                assert magpath.exists()
                magdata = mag.get("magnifier", mag)
                for row in magdata["users"]:
                    if row.get("overlay"):
                        gate.known_overlay_hwnds.add(int(row["overlay"]["hwnd"]))
                minfo = magdata.get("export", magdata)
                assert minfo.get("width", 64) == 64 and minfo.get("height", 64) == 64
                sample["magnifier_regression"] = {"metadata": minfo,
                    "pixels": gate.check_png(magpath, [64, 64], {})}
        if name == "capture-both-sample-fence":
            client = clients["full-a"]
            gate.set_delay(0, 4000)
            revoke_path = run / "revoked-before-commit.png"
            old_job = gate.composition_thread(client, "revoke-in-flight", 0, "all_users", revoke_path)
            gate.wait_pending("preparing")
            assert not old_job["done"].is_set(), "Revocation job finished before its policy Apply"
            revoked_worker_started = True
            sample["revoke_pending"] = ui.read_json(run / "ui.annotation.json").get("export_pending")
        if name == "capture-revoked":
            assert revoked_worker_started and gate.async_call
            snap = actual_settings(sample, ["anno-a"],
                [proc.pid for proc in gate.probes_owned], False)
            assert snap["selected_users"] == ["anno-a"] and not snap["capture"], snap
            # Closing a client with an in-flight RPC waits for that job. Keep
            # retired clients alive until completion and connect a fresh peer.
            clients["after-revoke"] = gate.open_mcp("after-revoke")
            fresh_after_revoke = clients["after-revoke"]
            sample["committed_policy"] = committed_policy(fresh_after_revoke,
                ["anno-a"], [proc.pid for proc in gate.probes_owned], False)
            pending = ui.read_json(run / "ui.annotation.json").get("export_pending")
            assert pending, "Old export must remain pending through policy revocation"
            destination, busy = gate.export(fresh_after_revoke, "deny-new-while-old-busy", 0, "selected_user", error=True)
            assert "still in progress" in json.dumps(busy, ensure_ascii=False).lower(), busy
            gate.no_output(destination)
            old_job = gate.async_call
            assert old_job["done"].wait(35), "Revoked old job did not release after preparation ended"
            gate.no_output(old_job["path"])
            state = gate.state(fresh_after_revoke)
            state = ui.eventually(lambda: gate.state(fresh_after_revoke),
                lambda value: value.get("export_pending") is None, 12)
            assert not gate.report(1).get("held_keys")
            gate.clear_delay()
            gate.async_call = None
            sample["precommit_revocation"] = {"new_job_rejected_while_old_pending": True,
                "old_job_result_or_error": old_job["result"] if old_job["error"] is None else old_job["error"],
                "no_png_or_temp_after_release": True, "pending_cleared": True, "peer_holds_clear": True}
        if name == "stop":
            ui.eventually(lambda: [ui.user32.IsWindow(hwnd) for hwnd in overlays],
                lambda alive: not any(alive), 10)
            assert all(not report.get("held_keys") and not report.get("buttons") for report in gate.probes())
        if name == "home-stop":
            assert reused_local_gui or (default_gui_exported and selected_gui_exported)
        return True

    def before_controls(g):
        hwnd = g.main_hwnd()
        if not hwnd:
            return False
        if not getattr(g, "launch_ready", False):
            # Exact process instance is retained before its window is used.
            g.own_process_identity(g.gui, "gui", retain=True)
            hwnd = g.main_hwnd()
            assert hwnd
            g.once("resize-ui-initial-540x520", lambda: geometry.resize_owned_client(hwnd, [540, 520]))
            target_union = {"left": min(w["left"] for w in target_rects),
                "top": min(w["top"] for w in target_rects),
                "right": max(w["right"] for w in target_rects),
                "bottom": max(w["bottom"] for w in target_rects)}
            choices = [(display["left"] + 12, display["top"] + 12),
                (display["right"] - 552, display["top"] + 12),
                (display["left"] + 12, display["bottom"] - 532)]
            for x, y in choices:
                rect = {"left": x, "top": y, "right": x + 540, "bottom": y + 520}
                if rect["right"] <= target_union["left"] or rect["left"] >= target_union["right"] or \
                    rect["bottom"] <= target_union["top"] or rect["top"] >= target_union["bottom"]:
                    g.once("place-owned-gui", lambda: ui.user32.SetWindowPos(hwnd, wintypes.HWND(0), x, y, 0, 0, 0x0001 | 0x0004 | 0x0010))
                    break
            else:
                raise AssertionError("No owned GUI location is clear of both capture windows")
            g.launch_ready = True
        return True

    def after_controls(g):
        hwnd = g.main_hwnd()
        assert hwnd and g.gui.pid == g.gui_generation["pid"]
        if g.gui.poll() is None:
            g.once("close-owned-gui", lambda: ui.user32.PostMessageW(hwnd, 0x0010, 0, 0))
            g.gui.wait(timeout=12)
        assert g.gui.returncode == 0, g.gui.returncode
        assert not any(ui.user32.IsWindow(handle) for handle in overlays)
        assert not list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{g.gui.pid}-*.json"))
        g.expected_exit = True
        g.once("remove-startup-test-key", lambda: lifecycle.remove_startup_key(g.startup_key))
        assert lifecycle.startup_value(g.startup_key) is None
        for index, proc in enumerate(g.probes_owned):
            g.check_process(f"probe-{index}", proc)
            capture_hwnd = int(g.report(index)["capture_hwnd"])
            assert ui.user32.IsWindow(capture_hwnd)
            owner = wintypes.DWORD()
            ui.user32.GetWindowThreadProcessId(capture_hwnd, ctypes.byref(owner))
            assert owner.value == proc.pid
            hwnd = geometry.owned_window(proc.pid)
            assert hwnd and int(hwnd) == int(target_windows[index]["hwnd"])
            g.once(f"close-owned-probe-{index}", lambda hwnd=hwnd: ui.user32.PostMessageW(hwnd, 0x0010, 0, 0))
            proc.wait(timeout=10)
            assert proc.returncode == 0
        g.evidence["clean_exit"] = {"gui_returncode": g.gui.returncode,
            "probe_returncodes": [proc.returncode for proc in g.probes_owned], "descriptor_absent": True,
            "startup_test_key_absent": True, "overlays_absent": True}
        return True

    try:
        # Start and position both owned pattern probes before the GUI reads its
        # profile, binding their exact PIDs and starts into the initial profile.
        for i, rgb in enumerate(((70, 60, 90), (60, 80, 70))):
            own_probe(i, rgb)
        discovery = module("annotation_export_discovery", "test-mcp.py").Client(
            gate.images[0], args.runtime.resolve(), run / "placement.stderr.log")
        try:
            display = discovery.tool("routing_displays")["desktop"]
            inventory = ui.eventually(lambda: discovery.tool("routing_windows"),
                lambda value: all(any(item["pid"] == proc.pid for item in value["windows"])
                    for proc in gate.probes_owned), 12)
            target_windows = [next(item for item in inventory["windows"] if item["pid"] == proc.pid)
                for proc in gate.probes_owned]
            positions = [(display["right"] - 700, display["top"] + 24),
                (display["right"] - 700, display["top"] + 340),
                (display["left"] + 24, display["bottom"] - 620)]
            placed = []
            for x, y in positions:
                rect = {"left": x, "top": y, "right": x + 340, "bottom": y + 270}
                if rect["left"] < display["left"] or rect["top"] < display["top"] or \
                    rect["right"] > display["right"] or rect["bottom"] > display["bottom"]:
                    continue
                if all(rect["right"] <= row["left"] or rect["left"] >= row["right"] or
                    rect["bottom"] <= row["top"] or rect["top"] >= row["bottom"] for row in placed):
                    placed.append(rect)
                if len(placed) == 2:
                    break
            assert len(placed) == 2, "Desktop cannot fit two disposable capture windows"
            for i, (window, rect) in enumerate(zip(target_windows, placed)):
                hwnd = int(window["hwnd"])
                owner = wintypes.DWORD()
                ui.user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
                assert owner.value == gate.probes_owned[i].pid
                gate.once(f"place-owned-probe-{i}", lambda hwnd=hwnd, rect=rect: ui.user32.SetWindowPos(
                    hwnd, wintypes.HWND(0), rect["left"], rect["top"], 340, 270, 0x0004 | 0x0010))
            target_rects = placed
            target_windows = [next(item for item in discovery.tool("routing_windows")["windows"]
                if item["pid"] == proc.pid) for proc in gate.probes_owned]
        finally:
            discovery.close()
        for index, window in enumerate(target_windows):
            seats[index]["target_pid"] = window["pid"]
            seats[index]["desktop_start"] = [window["client"]["left"] + 24, window["client"]["top"] + 24]
        profile["desktop"]["allowed_pids"] = [proc.pid for proc in gate.probes_owned]
        A_POINT = [target_windows[0]["client"]["left"] + 100, target_windows[0]["client"]["top"] + 100]
        B_POINT = [target_windows[1]["client"]["left"] + 100, target_windows[1]["client"]["top"] + 100]
        for i, point in enumerate((A_POINT, B_POINT)):
            report = gate.report(i)
            capture_hwnd = int(report["capture_hwnd"])
            owner = wintypes.DWORD()
            ui.user32.GetWindowThreadProcessId(capture_hwnd, ctypes.byref(owner))
            assert owner.value == gate.probes_owned[i].pid
            local = wintypes.POINT(point[0], point[1])
            bounds = geometry.Rect()
            assert ui.user32.ScreenToClient(capture_hwnd, ctypes.byref(local))
            assert ui.user32.GetClientRect(capture_hwnd, ctypes.byref(bounds))
            assert bounds.left <= local.x < bounds.right and bounds.top <= local.y < bounds.bottom
            origin = wintypes.POINT(0, 0)
            assert ui.user32.ClientToScreen(capture_hwnd, ctypes.byref(origin))
            capture_rects.append({"left": origin.x, "top": origin.y,
                "right": origin.x + bounds.right - bounds.left,
                "bottom": origin.y + bounds.bottom - bounds.top,
                "hwnd": capture_hwnd, "pid": owner.value})
        both_pids = ",".join(str(proc.pid) for proc in gate.probes_owned)
        policy_fields = {"agent-off": both_pids, "capture-a": both_pids,
            "capture-a-pid-b": str(gate.probes_owned[1].pid), "capture-both": both_pids,
            "capture-revoked": both_pids}
        for row in steps:
            if row["id"] in policy_fields:
                row["fields"]["agent_pids"] = policy_fields[row["id"]]
        steps[0]["wait_ms"] = 1800
        ui.put(run / "profile.json", profile)
        gate.evidence["producer_freeze"] = producer
        gate.once("launch-and-drive-single-gui-session", lambda: (
            gate.execute(profile, preferences, steps, verify, before_controls=before_controls,
                after_controls=after_controls), {"session_completed": True})[1])
        evidence["result"] = "PASS"
        evidence["limits"] = ["No timeout-after-approval OS failure injection was attempted.",
            "No target-delivery claim beyond exact disposable-window counters and decoded owned PNGs."]
        gate.save()
        return run
    except BaseException as error:
        evidence["failure"] = repr(error)
        evidence["result"] = "NOT ACCEPTED"
        gate.save()
        raise
    finally:
        gate.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--producer", type=Path, required=True)
    parser.add_argument("--producer-sha256", required=True)
    parser.add_argument("--ready", action="store_true")
    parser.add_argument("--resume-prefix", type=Path)
    parser.add_argument("--resume-gui-prefix", type=Path)
    parser.add_argument("--resume-connected-prefix", type=Path)
    args = parser.parse_args()
    assert args.ready, "Await root frozen-image READY/exclusive slot"
    assert len(args.producer_sha256) == 64 and all(c in "0123456789abcdef" for c in args.producer_sha256)
    assert digest(args.producer) == args.producer_sha256, "Producer report hash changed after freeze"
    workflow(args)


if __name__ == "__main__":
    main()
