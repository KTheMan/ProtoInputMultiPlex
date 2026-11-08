"""Finite owned GUI gate; run only after coordinating the exclusive native slot.

Actual controls use the application's existing --ui-script egui input hook.
There is no SendInput/global pointer automation and no direct policy-file edit.
The Rust live_gui test owns all MCP traffic. Descriptors stay in memory/env.
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
import sys
import tempfile
import time
import uuid

IDS = ["rust-client-gui-a", "rust-client-gui-b"]
NO_WINDOW = subprocess.CREATE_NO_WINDOW
user32 = ctypes.WinDLL("user32", use_last_error=True)
CALLBACK = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
user32.EnumWindows.argtypes = [CALLBACK, wintypes.LPARAM]
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
user32.IsWindowVisible.argtypes = [wintypes.HWND]
user32.GetWindow.argtypes = [wintypes.HWND, wintypes.UINT]
user32.GetWindow.restype = wintypes.HWND
user32.GetClientRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
user32.ClientToScreen.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.POINT)]
user32.SetWindowPos.argtypes = [wintypes.HWND, wintypes.HWND, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, wintypes.UINT]
user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, ctypes.c_size_t, ctypes.c_ssize_t]


class OwnedJob:
    """Own every descendant before a suspended process is allowed to execute."""
    def __init__(self):
        self.k = ctypes.WinDLL("kernel32", use_last_error=True)
        self.k.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
        self.k.CreateJobObjectW.restype = wintypes.HANDLE
        self.k.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
        self.k.QueryInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD, ctypes.c_void_p]
        self.k.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
        self.k.CloseHandle.argtypes = [wintypes.HANDLE]
        self.k.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
        self.k.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
        self.k.OpenThread.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        self.k.OpenThread.restype = wintypes.HANDLE
        self.k.ResumeThread.argtypes = [wintypes.HANDLE]
        self.k.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        self.k.OpenProcess.restype = wintypes.HANDLE
        self.k.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        class Basic(ctypes.Structure):
            _fields_ = [("process_time", ctypes.c_int64), ("job_time", ctypes.c_int64), ("flags", wintypes.DWORD), ("minimum", ctypes.c_size_t), ("maximum", ctypes.c_size_t), ("active", wintypes.DWORD), ("affinity", ctypes.c_size_t), ("priority", wintypes.DWORD), ("scheduling", wintypes.DWORD)]
        class Extended(ctypes.Structure):
            _fields_ = [("basic", Basic), ("io", ctypes.c_uint64 * 6), ("process_memory", ctypes.c_size_t), ("job_memory", ctypes.c_size_t), ("peak_process", ctypes.c_size_t), ("peak_job", ctypes.c_size_t)]
        self.handle = self.k.CreateJobObjectW(None, None)
        assert self.handle
        limits = Extended()
        limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        assert self.k.SetInformationJobObject(self.handle, 9, ctypes.byref(limits), ctypes.sizeof(limits))

    def launch(self, command, **options):
        process = subprocess.Popen(command, creationflags=NO_WINDOW | 4, **options)  # CREATE_SUSPENDED
        try:
            assert self.k.AssignProcessToJobObject(self.handle, int(process._handle))
            class Thread(ctypes.Structure):
                _fields_ = [("size", wintypes.DWORD), ("usage", wintypes.DWORD), ("thread", wintypes.DWORD), ("owner", wintypes.DWORD), ("base", wintypes.LONG), ("delta", wintypes.LONG), ("flags", wintypes.DWORD)]
            self.k.Thread32First.argtypes = [wintypes.HANDLE, ctypes.POINTER(Thread)]
            self.k.Thread32Next.argtypes = [wintypes.HANDLE, ctypes.POINTER(Thread)]
            snapshot = self.k.CreateToolhelp32Snapshot(4, 0)
            assert snapshot and snapshot != ctypes.c_void_p(-1).value
            threads = []
            try:
                entry = Thread(); entry.size = ctypes.sizeof(entry)
                more = self.k.Thread32First(snapshot, ctypes.byref(entry))
                while more:
                    if entry.owner == process.pid:
                        threads.append(entry.thread)
                    more = self.k.Thread32Next(snapshot, ctypes.byref(entry))
            finally:
                self.k.CloseHandle(snapshot)
            assert len(threads) == 1, "fresh suspended owned process must have one primary thread"
            thread = self.k.OpenThread(2, False, threads[0])
            assert thread
            try:
                assert self.k.ResumeThread(thread) == 1
            finally:
                self.k.CloseHandle(thread)
            return process
        except Exception:
            process.kill(); process.wait(timeout=10)
            raise

    def members(self):
        class Members(ctypes.Structure):
            _fields_ = [("assigned", wintypes.DWORD), ("count", wintypes.DWORD), ("ids", ctypes.c_size_t * 64)]
        values = Members()
        assert self.k.QueryInformationJobObject(self.handle, 3, ctypes.byref(values), ctypes.sizeof(values), None)
        assert values.count <= 64
        return list(values.ids[:values.count])

    def finish(self):
        deadline = time.monotonic() + 3
        while self.members() and time.monotonic() < deadline:
            time.sleep(.05)
        remaining = self.members()
        handles = [self.k.OpenProcess(0x100000, False, pid) for pid in remaining]
        assert self.k.CloseHandle(self.handle)
        self.handle = None
        # An inaccessible handle is unknown, never proof of termination.
        terminated = all(handle and self.k.WaitForSingleObject(handle, 10000) == 0 for handle in handles)
        for handle in handles:
            if handle:
                self.k.CloseHandle(handle)
        return {"owned_job_members_before_final_close": len(remaining), "owned_job_descendants_terminated": terminated}


def read(path):
    return json.loads(path.read_text(encoding="utf-8"))


def put(path, value):
    temporary = path.with_suffix(path.suffix + ".pending")
    temporary.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def eventually(operation, predicate, seconds=25):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            value = operation()
            if predicate(value):
                return value
        except (OSError, ValueError):
            pass
        time.sleep(.05)
    raise AssertionError("finite owned-state observation deadline")


def own_window(pid):
    windows = []
    @CALLBACK
    def visit(hwnd, unused):
        actual = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(actual))
        if actual.value == pid and user32.IsWindowVisible(hwnd) and not user32.GetWindow(hwnd, 4):
            windows.append(int(hwnd))
        return True
    assert user32.EnumWindows(visit, 0)
    return windows[0] if len(windows) == 1 else None


def own_close(process):
    if process is None or process.poll() is not None:
        return
    hwnd = own_window(process.pid)
    if hwnd:
        actual = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(actual))
        assert actual.value == process.pid
        assert user32.PostMessageW(hwnd, 0x10, 0, 0)
        try:
            process.wait(timeout=12)
            return
        except subprocess.TimeoutExpired:
            pass
    process.kill()
    process.wait(timeout=10)


def step(name, button, point=None):
    result = {"id": name, "button": button, "wait_ms": 400, "wait_for_sample": True}
    if point is not None:
        result.update(point=point, user=0)
    return result


def policy_summary(value):
    return {key: value.get(key) for key in ["selected_users", "allowed_pids", "capture", "session_control", "app_preferences", "policy_enabled", "clients", "pending"]} | {"has_error": value.get("error") is not None}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--run-root", type=Path, required=True)
    parser.add_argument("--native-slot-approved", action="store_true")
    args = parser.parse_args()
    assert args.native_slot_approved, "coordinate the native slot first"
    crate = Path(__file__).resolve().parent.parent
    run = args.run_root.resolve() / ("gui-" + time.strftime("%Y%m%d-%H%M%S"))
    run.mkdir(parents=True, exist_ok=False)
    controller = run / "rust_client_gui_control.exe"
    shutil.copy2(args.server.resolve(), controller)
    runtime = run / "runtime"
    runtime.mkdir()
    for name in ["ProtoInputLoader64.dll", "ProtoInputLoader32.dll", "ProtoInputHooks64.dll", "ProtoInputHooks32.dll", "ProtoInputIJP64.dll", "ProtoInputIJP32.dll", "ProtoInputIJ64.exe", "ProtoInputIJ32.exe"]:
        shutil.copy2(args.runtime.resolve() / name, runtime / name)
    # Script periodically captures only its own viewport. Prevent pixel files
    # from being saved: Agent Access renders the private descriptor path.
    (run / "ui.png").mkdir()
    sync = run / "sync"
    sync.mkdir()
    preferences = run / "preferences.json"
    prefs = {"version": 1, "launch_at_login": False, "launch_minimized": False, "minimize_to_tray": False, "close_to_tray": False, "shortcuts": [], "appearance_mode": "dark", "zoom_percent": 100}
    put(preferences, prefs)
    preferences_hash = hashlib.sha256(preferences.read_bytes()).hexdigest()
    result = {"result": "NOT ACCEPTED", "ui_observations": [], "policy_change": {"approved_source": "Explicit user request and coordinated main-session native slot", "delta": "Add second owned virtual user; PID and capture/session-control/app-preference grants unchanged"}, "server_sha256": hashlib.sha256(controller.read_bytes()).hexdigest(), "pixels_saved": False}
    probe = gui = rust = None
    job = OwnedJob()
    descriptor = None
    stage = "setup"
    try:
        probe = job.launch([str(controller), "probe", "--automation", "--report", str(run / "probe.json")], cwd=run, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        hwnd = eventually(lambda: own_window(probe.pid), bool)
        assert user32.SetWindowPos(hwnd, wintypes.HWND(-1), 40, 40, 640, 430, 0x10)
        origin = wintypes.POINT(0, 0)
        assert user32.ClientToScreen(hwnd, ctypes.byref(origin))
        point = [origin.x + 140, origin.y + 120]
        eventually(lambda: read(run / "probe.json"), lambda value: value["pid"] == probe.pid)
        profile = {"version": 1, "concurrent": True, "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": [probe.pid]}, "seats": [{"id": identity, "name": f"Rust GUI owned {index + 1}", "mouse": f"virtual://mouse/{index + 1}", "keyboard": f"virtual://keyboard/{index + 1}", "desktop_start": point, "cursor": {"lock_window": False}} for index, identity in enumerate(IDS)]}
        put(run / "profile.json", profile)
        steps = [step("start", "Start routing"), step("settings-input", "Settings"), step("input", "Input lab"), step("move", "1 Move to point", point), step("focus-down", "1 Hold click"), step("focus-up", "1 Release click"), step("home-policy", "Back to Home"), step("settings-policy", "Settings"), step("agent", "Agent Access"), step("user-a", "Agent user 1"), step("target", f"Agent target PID {probe.pid}"), step("enable", "Enable agent access"), step("user-b-draft", "Agent user 2"), step("apply", "Apply access policy"), step("disable", "Disable agent access"), step("home-stop", "Back to Home"), step("stop", "Stop routing")]
        put(run / "ui.json", steps)
        gui = job.launch([str(controller), "gui", "--profile", str(run / "profile.json"), "--preferences", str(preferences), "--startup-test-key", "Software\\ProtoInputMultiPlex\\Tests\\" + str(uuid.uuid4()), "--ui-script", str(run / "ui.json")], cwd=run, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        result.update(gui_pid=gui.pid, target_pid=probe.pid, expected_ui_controls=len(steps))
        for count, spec in enumerate(steps, 1):
            stage = spec["id"]
            def events():
                return [json.loads(line) for line in (run / "ui.events.jsonl").read_text(encoding="utf-8").splitlines()]
            actual = eventually(events, lambda values: len(values) >= count, 45)
            assert len(actual) == count
            assert actual[-1] == {"button": spec["button"], "clicked": True}
            eventually(lambda: read(run / "ui.ready.json"), bool)
            observation = {"id": stage, "button": spec["button"], "clicked": True}
            if stage == "focus-up":
                eventually(lambda: read(run / "ui.state.json"), lambda states: states and states[0]["pid"] == probe.pid and states[0]["keyboard_pid"] == probe.pid and not states[0]["buttons"])
            if stage == "agent":
                defaults = eventually(lambda: read(run / "ui.agent.json"), lambda value: "policy_enabled" in value)
                assert not any(defaults[key] for key in ["capture", "session_control", "app_preferences", "policy_enabled", "clients"])
                observation["default_policy"] = policy_summary(defaults)
            if stage == "enable":
                policy = eventually(lambda: read(run / "ui.agent.json"), lambda v: v["policy_enabled"] and not v["pending"] and v["selected_users"] == IDS[:1] and v["allowed_pids"] == [probe.pid])
                assert not any(policy[key] for key in ["capture", "session_control", "app_preferences"])
                matches = eventually(lambda: list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{gui.pid}-*.json")), lambda matches: len(matches) == 1)
                descriptor = matches[0]  # Kept private; never printed or serialized.
                env = os.environ.copy()
                env.update(PIM_TEST_SERVER=str(controller), PIM_TEST_RUNTIME=str(runtime), PIM_TEST_PREFERENCES=str(preferences), PIM_GUI_CONNECTION=str(descriptor), PIM_GUI_SLOT_APPROVED="1", PIM_GUI_TEST_PID=str(probe.pid), PIM_GUI_TEST_USER=IDS[0], PIM_GUI_TEST_X=str(point[0]), PIM_GUI_TEST_Y=str(point[1]), PIM_GUI_TEST_SYNC=str(sync), PIM_GUI_TEST_REPORT=str(run / "probe.json"), CARGO_TARGET_DIR=str(args.target_dir.resolve()), RUSTC_WRAPPER="")
                rust = job.launch(["cargo", "test", "--manifest-path", str(crate / "Cargo.toml"), "--locked", "--test", "live_gui", "--", "--ignored"], cwd=crate, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                held = eventually(lambda: read(sync / "held.json"), lambda v: v.get("hold_observed") is True, 75)
                observation["rust_prefix"] = held
            if stage == "user-b-draft":
                draft = eventually(lambda: read(run / "ui.agent.json"), lambda v: v["selected_users"] == IDS)
                report = read(run / "probe.json")
                assert 120 in report["held_keys"]
                assert held["old_client_id"] in draft["clients"]
                observation.update(draft_only_preserved_old_client=True, draft_only_preserved_hold=True)
            if stage == "apply":
                def acknowledged_policy():
                    lines = (run / "ui.agent.acks.jsonl").read_text(encoding="utf-8").splitlines()
                    return [json.loads(line) for line in lines]
                # The public Script ACK file contains AgentPage snapshots,
                # emitted only from agent_ack(..., acknowledged=true). It does
                # not expose Runtime State tags or an internal generation.
                acknowledgements = eventually(acknowledged_policy, lambda values: any(v["policy_enabled"] and v["selected_users"] == IDS and v["allowed_pids"] == [probe.pid] and not v["clients"] and not v["pending"] for v in values))
                ack = next(v for v in reversed(acknowledgements) if v["selected_users"] == IDS)
                assert not ack.get("error") and not any(ack[key] for key in ["capture", "session_control", "app_preferences"])
                released = eventually(lambda: read(run / "probe.json"), lambda v: not v["held_keys"] and v["buttons"] == 0)
                observation.update(policy_acknowledged=True, old_clients_cleared=not ack["clients"], hold_released_before_approval=True, released_key_event_count=released["key_event_count"])
                put(sync / "approved-applied.json", {"approved": True})
                outcome = eventually(lambda: read(sync / "result.json"), lambda v: v.get("result") in ["PASS", "FAIL"], 75)
                assert outcome["result"] == "PASS"
                rust.wait(timeout=12)
                assert rust.returncode == 0
                stdout, stderr = rust.communicate(timeout=5)
                # No child output is persisted. Detect any accidental descriptor
                # path leak even though the Rust test uses sanitized summaries.
                assert str(descriptor).encode() not in stdout + stderr
                observation["rust_result"] = outcome
                observation["gui_routing_continued_after_agent_disconnect"] = read(run / "ui.ready.json") is True
            if stage == "disable":
                disabled = eventually(lambda: read(run / "ui.agent.json"), lambda v: not v["policy_enabled"] and not v["pending"] and not v["clients"])
                eventually(lambda: descriptor.exists(), lambda exists: not exists)
                observation.update(policy_disabled=True, descriptor_removed_by_product=True)
            if stage == "stop":
                assert not any(read(run / "probe.json")[key] for key in ["held_keys", "buttons"])
                observation["native_holds_empty"] = True
            result["ui_observations"].append(observation)
            put(run / "gui-evidence.json", result)
            (run / "ui.sampled.json").write_text(str(count), encoding="utf-8")
            print(f"Accepted owned control {count}/{len(steps)}: {stage}", flush=True)
        assert preferences_hash == hashlib.sha256(preferences.read_bytes()).hexdigest()
        assert (run / "ui.png").is_dir()
        assert not [path for path in run.rglob("*.png") if path.is_file()]
        result.update(result="PASS", preferences_unchanged=True, capture_grant=False, session_control_grant=False, app_preferences_grant=False)
    except Exception as error:
        result.update(result="FAILED", failed_stage=stage, failure_type=type(error).__name__)
        # No repr/error text: a filesystem exception could include a descriptor.
        print(f"FAILED owned GUI stage {stage}: {type(error).__name__}", flush=True)
    finally:
        if rust is not None and rust.poll() is None:
            rust.kill()
            rust.wait(timeout=10)
        own_close(gui)
        own_close(probe)
        result["owned_direct_processes_reaped"] = all(p is None or p.poll() is not None for p in [rust, gui, probe])
        result["descriptor_absent_after_cleanup"] = descriptor is None or not descriptor.exists()
        result.update(job.finish())
        if result["result"] == "PASS" and (not result["owned_direct_processes_reaped"] or not result["descriptor_absent_after_cleanup"] or result["owned_job_members_before_final_close"] != 0 or not result["owned_job_descendants_terminated"]):
            result.update(result="FAILED", failed_stage="cleanup", failure_type="UnconfirmedOwnedCleanup")
        put(run / "gui-evidence.json", result)
        print(json.dumps({"result": result["result"], "run": str(run), "owned_direct_processes_reaped": result["owned_direct_processes_reaped"]}), flush=True)
    return 0 if result["result"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
