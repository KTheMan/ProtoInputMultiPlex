"""Bounded shared Ctrl+Alt+Shift+F11 acceptance; launch only after --ready."""
import argparse
import copy
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import time


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def freeze(args):
    """Bind the native run to a reviewed Rust producer; do not require a native rebuild."""
    assert args.ready, "Wait for final source review and ROOT READY before launching."
    assert digest(args.producer) == args.producer_sha256.lower()
    producer = json.loads(args.producer.read_text(encoding="utf-8"))
    assert producer["result"] == "PASS"
    assert producer["source_before_sha256"] == producer["source_after_sha256"]
    assert len(producer["checks"]) == 5 and all(item["exit_code"] == 0 for item in producer["checks"])
    root = Path(__file__).resolve().parent.parent
    baseline_path = root / "runtime/producer-clean-restart-20261015-095400/verification.json"
    baseline_sha256 = "2833cd9c02798af00a33fec0bbfacf6c29ebcea1bcb1cc03932a184988badae2"
    assert digest(baseline_path) == baseline_sha256, "Accepted native runtime baseline changed"
    baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    guard = producer["native_runtime_baseline"]
    assert Path(guard["path"]).resolve() == baseline_path.resolve()
    assert guard["sha256"] == baseline_sha256
    assert guard["runtime_sha256"] == baseline["runtime_sha256"]
    assert producer["runtime_sha256"] == baseline["runtime_sha256"]
    assert producer["doctor"]["exit_code"] == 0
    assert all(digest(root / name) == value
               for name, value in producer["source_after_sha256"].items())
    assert all(digest(args.runtime / name) == value
               for name, value in producer["runtime_sha256"].items())
    assert digest(args.executable) == producer["build_sha256"]["x64"]
    return producer


def load_module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def native(args, producer):
    helper = load_module("shared_stop_mcp", "test-mcp.py")
    geometry = load_module("shared_stop_geometry", "owned-gui-geometry.py")
    user32 = geometry.user32
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)

    user32.RegisterHotKey.argtypes = [wintypes.HWND, ctypes.c_int, wintypes.UINT, wintypes.UINT]
    user32.RegisterHotKey.restype = wintypes.BOOL
    user32.UnregisterHotKey.argtypes = [wintypes.HWND, ctypes.c_int]
    user32.UnregisterHotKey.restype = wintypes.BOOL
    user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
    user32.PostMessageW.restype = wintypes.BOOL
    user32.FindWindowExW.argtypes = [wintypes.HWND, wintypes.HWND, wintypes.LPCWSTR, wintypes.LPCWSTR]
    user32.FindWindowExW.restype = wintypes.HWND
    user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
    user32.GetClassNameW.restype = ctypes.c_int
    user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
    user32.SetWindowPos.argtypes = [wintypes.HWND, wintypes.HWND, ctypes.c_int, ctypes.c_int,
                                    ctypes.c_int, ctypes.c_int, wintypes.UINT]
    user32.IsWindow.argtypes = [wintypes.HWND]
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel.WaitForSingleObject.restype = wintypes.DWORD
    kernel.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    kernel.GetProcessTimes.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.FILETIME),
                                      ctypes.POINTER(wintypes.FILETIME), ctypes.POINTER(wintypes.FILETIME),
                                      ctypes.POINTER(wintypes.FILETIME)]
    kernel.QueryFullProcessImageNameW.argtypes = [wintypes.HANDLE, wintypes.DWORD,
                                                  wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD)]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]

    run = args.runtime.resolve() / time.strftime("acceptance-shared-stop-native-%Y%m%d-%H%M%S")
    run.mkdir(exist_ok=False)
    executable = run / "control.exe"
    shutil.copy2(args.executable, executable)
    assert digest(executable) == producer["build_sha256"]["x64"]
    evidence = {
        "result": "NOT ACCEPTED",
        "producer": str(args.producer.resolve()),
        "producer_sha256": digest(args.producer),
        "producer_source_sha256": producer["source_after_sha256"],
        "producer_runtime_sha256": producer["runtime_sha256"],
        "executable": str(executable.resolve()),
        "executable_sha256": digest(executable),
        "scope": "Owned virtual devices and disposable targets only; no SendInput, OS pointer, hardware, or RustDesk input.",
        "groups": {},
        "calls": [],
        "mutations": 0,
        "routing_input_calls": 0,
        "cleanup_errors": [],
    }
    clients, targets, workers, brokers, handles = [], [], [], [], []
    blocker_registered = False
    blocker_id = 0x6A31
    chord_flags = 0x0001 | 0x0002 | 0x0004 | 0x4000
    vk_f11 = 0x7A

    def save():
        (run / "evidence.json").write_text(json.dumps(evidence, indent=2, ensure_ascii=False) + "\n",
                                            encoding="utf-8")

    def call(client, name, arguments=None, error=False):
        mutating = name not in ("routing_state", "routing_windows")
        if mutating:
            evidence["mutations"] += 1
        if name == "routing_input":
            evidence["routing_input_calls"] += 1
        assert evidence["mutations"] <= 40 and evidence["routing_input_calls"] <= 24
        record = {"controller_pid": client.process.pid, "tool": name, "arguments": arguments or {},
                  "expected_error": error, "started_ns": time.monotonic_ns()}
        evidence["calls"].append(record)
        save()
        record["result"] = client.tool(name, arguments, error=error)
        record["finished_ns"] = time.monotonic_ns()
        save()
        return record["result"]

    def state(client):
        return call(client, "routing_state")

    def report(target):
        return helper.eventually(lambda: json.loads(target["report"].read_text(encoding="utf-8")),
                                 lambda value: value.get("frames", 0) > 0, 6)

    def key_events(target, vk):
        return [event for event in report(target)["native_events"]
                if event.get("kind") == "key" and event.get("code") == vk]

    def signature(owned_targets):
        return {item["name"]: report(item)["native_events"] for item in owned_targets}

    def quiet(owned_targets):
        frames = {item["name"]: report(item)["frames"] for item in owned_targets}
        helper.eventually(lambda: {item["name"]: report(item) for item in owned_targets},
                          lambda values: all(values[item["name"]]["frames"] >= frames[item["name"]] + 3
                                             for item in owned_targets), 6)

    def spawn_target(name, position):
        report_path = run / f"{name}.json"
        process = subprocess.Popen([str(executable), "probe", "--automation", "--report", str(report_path)],
                                   creationflags=subprocess.CREATE_NO_WINDOW)
        target = {"name": name, "process": process, "report": report_path}
        targets.append(target)
        initial = helper.eventually(lambda: json.loads(report_path.read_text(encoding="utf-8")),
                                    lambda value: value.get("frames", 0) > 2, 12)
        target["root"] = int(geometry.owned_window(process.pid))
        target["child"] = int(initial["capture_hwnd"])
        owner = wintypes.DWORD()
        assert user32.GetWindowThreadProcessId(target["root"], ctypes.byref(owner))
        assert owner.value == process.pid and user32.IsWindow(target["child"])
        assert user32.SetWindowPos(target["root"], wintypes.HWND(-1), *position, 420, 360, 0x10 | 0x40)
        report(target)
        return target

    def connect(name):
        client = helper.Client(executable, args.runtime.resolve(), run / f"{name}.stderr.log")
        clients.append(client)
        return client

    def process_query(kind, parent_pid=None):
        image = str(executable.resolve()).replace("'", "''")
        parent = "" if parent_pid is None else f" -and $_.ParentProcessId -eq {int(parent_pid)}"
        command = ("$image='" + image + "'; @(Get-CimInstance Win32_Process | Where-Object { "
                   f"$_.ExecutablePath -ieq $image -and $_.CommandLine -like '*{kind}*'{parent}"
                   " } | Select-Object ProcessId,ParentProcessId,ExecutablePath,CommandLine,CreationDate) "
                   "| ConvertTo-Json -Compress")
        result = subprocess.run(["powershell", "-NoProfile", "-Command", command], capture_output=True,
                                text=True, timeout=15)
        assert result.returncode == 0, result.stderr
        rows = json.loads(result.stdout) if result.stdout.strip() else []
        return [rows] if isinstance(rows, dict) else rows

    def identity_handle(row, kind, terminate=False):
        pid = int(row["ProcessId"])
        access = 0x00100000 | 0x1000 | (0x0001 if terminate else 0)
        handle = kernel.OpenProcess(access, False, pid)
        assert handle
        item = {"pid": pid, "handle": handle, "terminate_right": terminate,
                "kind": kind, "command_line": row["CommandLine"],
                "cim_creation": row["CreationDate"], "handle_image": None,
                "creation_filetime": None}
        # Retain immediately, before any identity assertions can fail, so finally can close it.
        handles.append(item)
        assert kernel.WaitForSingleObject(handle, 0) == 258
        times = (wintypes.FILETIME * 4)()
        assert kernel.GetProcessTimes(handle, *[ctypes.byref(item) for item in times])
        creation = (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime
        image = ctypes.create_unicode_buffer(32768)
        length = wintypes.DWORD(len(image))
        assert kernel.QueryFullProcessImageNameW(handle, 0, image, ctypes.byref(length))
        assert Path(image.value).resolve() == executable.resolve()
        current = [candidate for candidate in process_query(kind)
                   if int(candidate["ProcessId"]) == pid]
        assert len(current) == 1
        current = current[0]
        assert current["CommandLine"] == row["CommandLine"]
        assert current["CreationDate"] == row["CreationDate"]
        assert Path(current["ExecutablePath"]).resolve() == executable.resolve()
        assert kernel.WaitForSingleObject(handle, 0) == 258
        item.update(creation_filetime=creation, handle_image=image.value)
        return item

    def revalidate_identity(item, kind):
        handle = item["handle"]
        assert kernel.WaitForSingleObject(handle, 0) == 258, item
        times = (wintypes.FILETIME * 4)()
        assert kernel.GetProcessTimes(handle, *[ctypes.byref(value) for value in times])
        creation = (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime
        assert creation == item["creation_filetime"], item
        image = ctypes.create_unicode_buffer(32768)
        length = wintypes.DWORD(len(image))
        assert kernel.QueryFullProcessImageNameW(handle, 0, image, ctypes.byref(length))
        assert Path(image.value).resolve() == executable.resolve() == Path(item["handle_image"]).resolve()
        rows = [row for row in process_query(kind) if int(row["ProcessId"]) == item["pid"]]
        assert len(rows) == 1, rows
        row = rows[0]
        assert row["CommandLine"] == item["command_line"]
        assert row["CreationDate"] == item["cim_creation"]
        assert Path(row["ExecutablePath"]).resolve() == executable.resolve()
        assert kernel.WaitForSingleObject(handle, 0) == 258, item
        return row

    def worker(client):
        rows = helper.eventually(lambda: process_query("session-worker", client.process.pid),
                                 lambda values: len(values) == 1, 12)
        row = rows[0]
        assert int(row["ParentProcessId"]) == client.process.pid
        assert Path(row["ExecutablePath"]).resolve() == executable.resolve()
        assert "session-worker" in row["CommandLine"]
        item = identity_handle(row, "session-worker")
        item["controller_pid"] = client.process.pid
        item["command_line"] = row["CommandLine"]
        item["cim_creation"] = row["CreationDate"]
        workers.append(item)
        return item

    def broker_records():
        rows = process_query("emergency-broker")
        found = []
        for row in rows:
            assert Path(row["ExecutablePath"]).resolve() == executable.resolve()
            pid = int(row["ProcessId"])
            item = next((candidate for candidate in brokers if candidate["pid"] == pid), None)
            if item is None:
                item = identity_handle(row, "emergency-broker", terminate=True)
                brokers.append(item)
            found.append(item)
        return found

    def wait_exit(item, expected=0):
        assert kernel.WaitForSingleObject(item["handle"], 15000) == 0
        code = wintypes.DWORD()
        assert kernel.GetExitCodeProcess(item["handle"], ctypes.byref(code))
        item["exit_code"] = code.value
        assert code.value == expected, item
        save()
        return code.value

    def broker_window(broker):
        parent = wintypes.HWND(-3)  # HWND_MESSAGE
        previous = wintypes.HWND()
        found = []
        while True:
            hwnd = user32.FindWindowExW(parent, previous, None, None)
            if not hwnd:
                break
            previous = hwnd
            class_name = ctypes.create_unicode_buffer(256)
            if not user32.GetClassNameW(hwnd, class_name, len(class_name)):
                continue
            if not class_name.value.startswith("ProtoInputMultiPlex.EmergencyStop.v1."):
                continue
            owner = wintypes.DWORD()
            assert user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
            if owner.value == broker["pid"]:
                found.append({"hwnd": int(hwnd), "class": class_name.value, "pid": owner.value})
        assert len(found) == 1, found
        return found[0]

    def validate_broker_window(broker):
        saved = broker.get("owned_broker_window")
        assert saved and user32.IsWindow(saved["hwnd"]), broker
        owner = wintypes.DWORD()
        assert user32.GetWindowThreadProcessId(saved["hwnd"], ctypes.byref(owner))
        assert owner.value == broker["pid"] == saved["pid"]
        class_name = ctypes.create_unicode_buffer(256)
        assert user32.GetClassNameW(saved["hwnd"], class_name, len(class_name))
        assert class_name.value == saved["class"]
        assert class_name.value.startswith("ProtoInputMultiPlex.EmergencyStop.v1.")
        return saved

    def route_start(client, target, name):
        result = call(client, "routing_start", {
            "users": [name], "user_ids": ["shared-stop-" + name],
            "allowed_pids": [target["process"].pid], "mode": "concurrent", "multi_keyboard": True,
        })
        assert result["running"] and len(result["users"]) == 1
        return result

    def focus(client, target):
        rect = geometry.Rect()
        assert user32.GetWindowRect(target["child"], ctypes.byref(rect))
        x, y = rect.left + 85, rect.top + 65
        assert rect.left <= x < rect.right and rect.top <= y < rect.bottom
        call(client, "routing_input", {"action": "move_to", "user": 0, "x": x, "y": y})
        call(client, "routing_input", {"action": "button", "user": 0, "button": "primary", "pressed": True})
        call(client, "routing_input", {"action": "button", "user": 0, "button": "primary", "pressed": False})
        selected = helper.eventually(lambda: state(client),
                                     lambda value: value["users"][0]["keyboard"]["clicked"]
                                     and value["users"][0]["keyboard"]["clicked"]["hwnd"] == target["child"], 10)
        overlay = int(selected["users"][0]["overlay"]["hwnd"])
        assert overlay and user32.IsWindow(overlay)
        target["overlay"] = overlay
        return selected

    def keyboard_settings(client):
        current = state(client)
        candidate = copy.deepcopy(current["user_settings"][0]["settings"])
        candidate["keyboard"] = {"enabled": True, "focus": "clicked"}
        result = call(client, "routing_settings", {"user": 0, "settings": candidate})
        assert result["user_settings"][0]["settings"] == candidate
        return result

    def key(client, name, pressed):
        return call(client, "routing_input", {"action": "key", "user": 0, "key": name,
                                               "pressed": pressed})

    def await_keys(target, vk, expected):
        return helper.eventually(lambda: key_events(target, vk),
                                 lambda values: [item["pressed"] for item in values] == expected, 10)

    def ordinary_stop(client, worker_identity, overlay):
        result = call(client, "routing_stop")
        assert not result["running"]
        stopped = helper.eventually(lambda: state(client),
                                    lambda value: not value["running"]
                                    and value["recovery"]["phase"] == "stopped"
                                    and not value["users"], 12)
        wait_exit(worker_identity)
        assert not overlay or not user32.IsWindow(overlay)
        return {"stop": result, "state": stopped}

    try:
        archive = run / "runner-source"
        archive.mkdir()
        for filename in (Path(__file__).name, "test-mcp.py", "owned-gui-geometry.py"):
            shutil.copy2(Path(__file__).with_name(filename), archive / filename)
        dependencies = {path.name: digest(path) for path in Path(__file__).parent.glob("*.py")}
        (archive / "all-python-dependency-hashes.json").write_text(
            json.dumps(dependencies, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        evidence["runner_dependency_sha256"] = dependencies
        evidence["copied_executable_metadata"] = {
            "path": str(executable.resolve()), "size": executable.stat().st_size,
            "modified_ns": executable.stat().st_mtime_ns,
        }
        save()

        target_a = spawn_target("target-a", (40, 45))
        target_b = spawn_target("target-b", (500, 45))
        target_c = spawn_target("target-c", (40, 440))
        owned_targets = [target_a, target_b, target_c]

        # An owned thread holds the fixed chord; startup must fail before worker attachment.
        assert user32.RegisterHotKey(None, blocker_id, chord_flags, vk_f11)
        blocker_registered = True
        blocked_client = connect("blocked-by-owned-fixture")
        before = signature(owned_targets)
        blocked_error = call(blocked_client, "routing_start", {
            "users": ["must-not-attach"], "user_ids": ["shared-stop-conflict"],
            "allowed_pids": [target_a["process"].pid], "mode": "concurrent", "multi_keyboard": True,
        }, error=True)
        assert not process_query("session-worker", blocked_client.process.pid)
        quiet(owned_targets)
        assert signature(owned_targets) == before
        conflict_text = " ".join(item.get("text", "") for item in blocked_error.get("content", []))
        assert "emergency" in conflict_text.lower() and "f11" in conflict_text.lower(), blocked_error
        evidence["groups"]["unrelated_chord_owner_fails_before_attach"] = {
            "fixture": {"owner": "this harness thread", "id": blocker_id,
                        "modifiers": chord_flags, "virtual_key": vk_f11},
            "routing_start_error": blocked_error, "worker_processes": [],
            "target_trace_unchanged": True,
        }
        save()
        assert user32.UnregisterHotKey(None, blocker_id)
        blocker_registered = False
        blocked_client.close()
        clients.remove(blocked_client)

        client_a = connect("session-a")
        client_b = connect("session-b")
        route_start(client_a, target_a, "User A")
        worker_a = worker(client_a)
        focus(client_a, target_a)
        keyboard_settings(client_a)
        route_start(client_b, target_b, "User B")
        worker_b = worker(client_b)
        focus(client_b, target_b)
        keyboard_settings(client_b)
        broker = helper.eventually(broker_records, lambda rows: len(rows) == 1, 12)[0]
        hwnd = helper.eventually(lambda: broker_window(broker), lambda value: value is not None, 8)
        broker["owned_broker_window"] = hwnd
        # The actual broker registration excludes a second owner of the same chord.
        assert not user32.RegisterHotKey(None, blocker_id, chord_flags, vk_f11)
        assert ctypes.get_last_error() == 1409

        key(client_a, "f6", True)
        key(client_b, "f7", True)
        a_down = await_keys(target_a, 0x75, [True])
        b_down = await_keys(target_b, 0x76, [True])
        assert a_down[0]["hwnd"] == target_a["child"]
        assert b_down[0]["hwnd"] == target_b["child"]

        client_c = connect("busy-session-c")
        c_inventory = call(client_c, "routing_windows")["windows"]
        a_eligibility = next(row["eligibility"] for row in c_inventory
                             if row["pid"] == target_a["process"].pid)
        assert a_eligibility["state"] == "busy" and a_eligibility["reason"]
        route_start(client_c, target_a, "Busy C")
        worker_c = worker(client_c)
        c_before_busy = state(client_c)
        c_overlay = int(c_before_busy["users"][0]["overlay"]["hwnd"])
        assert c_overlay and user32.IsWindow(c_overlay)
        before_busy = signature(owned_targets)
        rect = geometry.Rect()
        assert user32.GetWindowRect(target_a["child"], ctypes.byref(rect))
        call(client_c, "routing_input", {"action": "move_to", "user": 0,
                                         "x": rect.left + 85, "y": rect.top + 65})
        busy_ack = call(client_c, "routing_input", {"action": "button", "user": 0,
                                                     "button": "primary", "pressed": True})
        busy = helper.eventually(lambda: state(client_c),
                                 lambda value: len(value["users"]) == 1
                                 and value["users"][0]["status"]
                                 == "Routing unavailable: " + a_eligibility["reason"], 10)
        assert not busy["users"][0]["buttons"] and not busy["users"][0]["keys"]
        assert not busy["users"][0]["captured"]
        quiet(owned_targets)
        assert signature(owned_targets) == before_busy
        assert key_events(target_a, 0x75) == a_down and key_events(target_b, 0x76) == b_down
        c_stop = ordinary_stop(client_c, worker_c, c_overlay)
        client_c.close()
        clients.remove(client_c)

        broker_after_c = helper.eventually(broker_records,
                                           lambda rows: len(rows) == 1 and rows[0]["pid"] == broker["pid"], 5)[0]
        a_stop = ordinary_stop(client_a, worker_a, target_a["overlay"])
        a_release = await_keys(target_a, 0x75, [True, False])
        assert all(item["hwnd"] == target_a["child"] for item in a_release)
        assert key_events(target_b, 0x76) == b_down, "Stopping A must leave B's F7 hold untouched"
        client_a.close()  # EOF after explicit Stop must not own the broker lifetime.
        clients.remove(client_a)
        broker_after_a_eof = helper.eventually(broker_records,
                                               lambda rows: len(rows) == 1 and rows[0]["pid"] == broker["pid"], 5)[0]
        hwnd_after_a_eof = broker_window(broker_after_a_eof)
        assert hwnd_after_a_eof == hwnd and user32.IsWindow(hwnd["hwnd"])
        assert key_events(target_b, 0x76) == b_down, "B's F7 hold must survive A's EOF"

        f5_before = len(key_events(target_b, 0x74))
        key(client_b, "f5", True)
        key(client_b, "f5", False)
        f5_pair = helper.eventually(lambda: key_events(target_b, 0x74),
                                    lambda values: len(values) == f5_before + 2, 8)[f5_before:]
        assert [item["pressed"] for item in f5_pair] == [True, False]
        assert all(item["hwnd"] == target_b["child"] for item in f5_pair)

        client_a2 = connect("session-a-after-eof")
        route_start(client_a2, target_c, "User A2")
        worker_a2 = worker(client_a2)
        focus(client_a2, target_c)
        keyboard_settings(client_a2)
        assert broker_records()[0]["pid"] == broker["pid"]
        key(client_a2, "f9", True)
        c_down = await_keys(target_c, 0x78, [True])
        assert c_down[0]["hwnd"] == target_c["child"]

        # Deliver only the exact WM_HOTKEY to the verified broker message-only HWND.
        hotkey_lparam = (vk_f11 << 16) | 0x0001 | 0x0002 | 0x0004
        assert user32.PostMessageW(hwnd["hwnd"], 0x0312, 0x504D, hotkey_lparam)
        stopped = helper.eventually(lambda: [state(client_b), state(client_a2)],
                                    lambda values: all(not value["running"]
                                                       and value["recovery"]["phase"] == "stopped"
                                                       and not value["users"] for value in values), 15)
        wait_exit(worker_b)
        wait_exit(worker_a2)
        b_release = await_keys(target_b, 0x76, [True, False])
        c_release = await_keys(target_c, 0x78, [True, False])
        assert all(item["hwnd"] == target_b["child"] for item in b_release)
        assert all(item["hwnd"] == target_c["child"] for item in c_release)
        assert all(not user32.IsWindow(item.get("overlay", 0))
                   for item in owned_targets if item.get("overlay"))
        assert all(item["process"].poll() is None for item in owned_targets)
        broker_exit = wait_exit(broker)
        assert user32.RegisterHotKey(None, blocker_id, chord_flags, vk_f11)
        blocker_registered = True
        assert user32.UnregisterHotKey(None, blocker_id)
        blocker_registered = False
        evidence["groups"]["shared_broker_lifetime_busy_isolation_and_stop"] = {
            "broker": {key: value for key, value in broker.items() if key != "handle"},
            "message_window": hwnd,
            "A_worker": {key: value for key, value in worker_a.items() if key != "handle"},
            "B_worker": {key: value for key, value in worker_b.items() if key != "handle"},
            "C_worker": {key: value for key, value in worker_c.items() if key != "handle"},
            "C_busy_ack": busy_ack, "C_busy_state": busy, "C_clean_stop": c_stop,
            "A_clean_stop_then_EOF": a_stop, "A_saved_recipient_F6": a_release,
            "B_F5_continuity_after_A_EOF": f5_pair,
            "same_broker_after_C_exit": broker_after_c["pid"] == broker["pid"],
            "same_broker_after_A_EOF": broker_after_a_eof["pid"] == broker["pid"],
            "A2_worker": {key: value for key, value in worker_a2.items() if key != "handle"},
            "posted_exact_message": {"message": 0x0312, "id": 0x504D, "lparam": hotkey_lparam,
                                     "hwnd": hwnd},
            "B_and_A2_emergency_stopped_states": stopped,
            "B_saved_recipient_F7": b_release, "A2_saved_recipient_F9": c_release,
            "broker_exit_after_last_lease": broker_exit,
            "both_targets_alive_after_stops": [item["process"].pid for item in owned_targets],
            "released_holds_and_closed_overlays": True,
        }
        save()
        client_b.close()
        clients.remove(client_b)
        client_a2.close()
        clients.remove(client_a2)
        evidence["result"] = "PENDING INDEPENDENT NATIVE DATA REVIEW"
        save()
    except Exception as error:
        evidence["failure"] = repr(error)
        save()
        raise
    finally:
        if blocker_registered and not user32.UnregisterHotKey(None, blocker_id):
            evidence["cleanup_errors"].append(f"fixture UnregisterHotKey failed: {ctypes.get_last_error()}")
        blocker_registered = False
        for client in list(clients):
            try:
                if client.process.poll() is None:
                    try:
                        current = call(client, "routing_state")
                        if current.get("running"):
                            call(client, "routing_stop")
                    except Exception as error:
                        evidence["cleanup_errors"].append(f"controller {client.process.pid} stop: {error!r}")
                client.close()
            except Exception as error:
                evidence["cleanup_errors"].append(f"controller {client.process.pid} close: {error!r}")
                if client.process.poll() is None:
                    client.process.kill()
                    client.process.wait(timeout=10)
                client.stderr.close()
        for target in targets:
            try:
                process = target["process"]
                if process.poll() is None:
                    assert user32.PostMessageW(target["root"], 0x0010, 0, 0)
                    process.wait(timeout=10)
                assert process.returncode == 0
            except Exception as error:
                evidence["cleanup_errors"].append(f"{target['name']} close: {error!r}")
        try:
            broker_records()
        except Exception as error:
            evidence["cleanup_errors"].append(f"owned broker inventory: {error!r}")
        for broker in brokers:
            if kernel.WaitForSingleObject(broker["handle"], 0) != 0:
                try:
                    revalidate_identity(broker, "emergency-broker")
                    if not broker.get("owned_broker_window"):
                        broker["owned_broker_window"] = broker_window(broker)
                    validate_broker_window(broker)
                    # Only terminate after the retained handle, current CIM identity, and
                    # broker-owned message-only HWND still agree on this exact run process.
                    assert kernel.TerminateProcess(broker["handle"], 1)
                    assert kernel.WaitForSingleObject(broker["handle"], 10000) == 0
                    broker["cleanup_terminated"] = True
                    evidence["cleanup_errors"].append(
                        f"owned broker {broker['pid']} required forced cleanup")
                except Exception as error:
                    evidence["cleanup_errors"].append(
                        f"owned broker {broker['pid']} identity-safe cleanup: {error!r}")
            code = wintypes.DWORD()
            if kernel.GetExitCodeProcess(broker["handle"], ctypes.byref(code)):
                broker.setdefault("exit_code", code.value)
        for item in workers:
            if kernel.WaitForSingleObject(item["handle"], 0) == 0:
                code = wintypes.DWORD()
                if kernel.GetExitCodeProcess(item["handle"], ctypes.byref(code)):
                    item.setdefault("exit_code", code.value)
            else:
                evidence["cleanup_errors"].append(f"worker {item['pid']} survived cleanup")
        for item in handles:
            kernel.CloseHandle(item["handle"])
        evidence["owned_worker_exits"] = {str(item["pid"]): item.get("exit_code") for item in workers}
        evidence["owned_broker_exits"] = {str(item["pid"]): item.get("exit_code") for item in brokers}
        evidence["known_overlays_closed"] = all(not user32.IsWindow(item["overlay"])
                                                 for item in targets if item.get("overlay"))
        evidence["owned_target_exit_codes"] = {str(item["process"].pid): item["process"].returncode
                                                for item in targets}
        try:
            survivors = process_query("session-worker") + process_query("emergency-broker")
        except Exception as error:
            survivors = []
            evidence["cleanup_errors"].append(f"scoped CIM cleanup audit: {error!r}")
        evidence["scoped_cim_survivors"] = survivors
        if (evidence["cleanup_errors"] or survivors
                or any(item.get("exit_code") != 0 for item in workers)
                or any(item.get("exit_code") != 0 for item in brokers)
                or any(item.get("cleanup_terminated") for item in brokers)):
            evidence["result"] = "NOT ACCEPTED"
        if any(code != 0 for code in evidence["owned_target_exit_codes"].values()):
            evidence["result"] = "NOT ACCEPTED"
        save()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready", action="store_true")
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--producer", type=Path, required=True)
    parser.add_argument("--producer-sha256", required=True)
    args = parser.parse_args()
    producer = freeze(args)
    native(args, producer)


if __name__ == "__main__":
    main()
