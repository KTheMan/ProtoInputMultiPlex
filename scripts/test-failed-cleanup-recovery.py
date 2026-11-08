"""Finite shared recovery gate; source preparation only until frozen ROOT READY."""
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
import sys
import time


def module(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def put(path, value):
    Path(path).write_text(json.dumps(value, indent=2), encoding="utf-8")


def read(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def freeze(args):
    assert args.ready, "Await final source approval, producer and ROOT READY"
    assert digest(args.producer) == args.producer_sha256.lower()
    p = read(args.producer)
    assert p["result"] == "PASS"
    repo = Path(__file__).resolve().parent.parent
    assert all(digest(repo / n) == h for n, h in p["source_after_sha256"].items())
    assert digest(args.executable) == p["build_sha256"]["x64"]
    assert digest(args.x86_executable) == p["build_sha256"]["i686"]
    assert all(digest(args.runtime / n) == h for n, h in p["runtime_sha256"].items())
    return p


class Fixtures:
    def __init__(self, folder, image, runtime):
        self.folder, self.image, self.runtime = folder, image, runtime
        self.helper = module("recovery_current_client", "test-mcp.py")
        self.pump = module("recovery_owned_pump", "test-output-shutdown-timeout.py")
        self.owned, self.log = [], None
        self.windows, self.overlays = [], set()

    def start(self, discovery):
        self.log = (self.folder / "pump.stderr.log").open("w", encoding="utf-8")
        self.a_report = self.folder / "pump.json"
        self.c_report = self.folder / "fresh.json"
        self.a = subprocess.Popen([sys.executable, str(Path(__file__).with_name("test-output-shutdown-timeout.py").resolve()),
                                   "--fixture", "--report", str(self.a_report)],
                                  stderr=self.log, creationflags=subprocess.CREATE_NO_WINDOW)
        self.owned.append(self.a)
        self.c = subprocess.Popen([str(self.image), "probe", "--automation", "--report", str(self.c_report)],
                                  creationflags=subprocess.CREATE_NO_WINDOW)
        self.owned.append(self.c)
        self.helper.eventually(lambda: read(self.a_report), lambda s: s["hwnd"] and s["frames"] > 2)
        self.helper.eventually(lambda: read(self.c_report), lambda s: s["frames"] > 2)
        pids = [self.a.pid, self.c.pid]
        def inventory():
            rows = discovery.tool("routing_windows")["windows"]
            return [next((w for w in rows if w["pid"] == pid and w["title"].startswith("ProtoInputMultiPlex Automation Test")), None) for pid in pids]
        self.windows = self.helper.eventually(inventory, lambda ws: all(ws))
        for i, w in enumerate(self.windows):
            assert self.helper.user32.SetWindowPos(w["hwnd"], wintypes.HWND(-1), 30 + i * 460, 40, 420, 360, 0x10)
        self.windows = self.helper.eventually(inventory, lambda ws: all(ws))
        self.points = [[w["client"]["left"] + 120, w["client"]["top"] + 100] for w in self.windows]
        for process, w in zip(self.owned, self.windows):
            pid = wintypes.DWORD()
            assert self.pump.user32.GetWindowThreadProcessId(w["hwnd"], ctypes.byref(pid)) and pid.value == process.pid

    def pause_pump(self, mode):
        self.pump.put(self.a_report.with_suffix(".mode.json"), {"mode": mode, "milliseconds": 3500})
        return self.helper.eventually(lambda: read(self.a_report), lambda s: s["mode_entered"] == 1 and s["pump_mode"] == mode)

    def late_release(self):
        s = self.helper.eventually(lambda: read(self.a_report), lambda s: s["pump_mode"] == "normal" and len(s["key_events"]) >= 2, 8)
        assert [(e["vk"], e["pressed"]) for e in s["key_events"]] == [(117, True), (117, False)]
        assert not s["held_app_keys"]
        return s

    def close_a(self):
        assert self.a.poll() is None
        assert self.pump.user32.PostMessageW(self.windows[0]["hwnd"], 0x10, 0, 0)
        self.a.wait(timeout=8)
        assert self.a.returncode == 0
        return {"pid": self.a.pid, "exit_code": self.a.returncode}

    def close(self):
        errors = []
        for process in reversed(self.owned):
            if process.poll() is None:
                try:
                    w = self.windows[self.owned.index(process)]
                    assert self.pump.user32.PostMessageW(w["hwnd"], 0x10, 0, 0)
                    process.wait(timeout=8)
                except Exception as error:
                    errors.append(repr(error))
                    if process.poll() is None:
                        process.terminate()
                        process.wait(timeout=10)
        if self.log:
            self.log.close()
        return errors


def quarantine(snapshot, a, c):
    assert snapshot["phase"] == "quarantined" and not snapshot["can_start"] and snapshot["error"], snapshot
    targets = snapshot["targets"]
    assert {t["pid"] for t in targets} == {a} and c not in {t["pid"] for t in targets}, snapshot
    assert targets[0]["affected"] and targets[0]["status"] == "running", snapshot
    return snapshot


def standalone(args):
    helper = module("recovery_stdio", "test-mcp.py")
    folder = args.runtime.resolve() / time.strftime("acceptance-failed-cleanup-stdio-%Y%m%d-%H%M%S")
    folder.mkdir()
    image = folder / "control64.exe"
    shutil.copy2(args.executable, image)
    f = Fixtures(folder, image, args.runtime.resolve())
    sources = folder / "runner-source"
    sources.mkdir()
    for name in (Path(__file__).name, "test-mcp.py", "test-output-shutdown-timeout.py"):
        shutil.copy2(Path(__file__).with_name(name), sources / name)
    evidence = {"result": "NOT ACCEPTED", "calls": [], "groups": {}, "producer": str(args.producer),
                "producer_sha256": digest(args.producer)}
    client = None
    def save(): put(folder / "evidence.json", evidence)
    def call(name, arguments=None, error=False):
        entry = {"name": name, "arguments": arguments or {}, "expected_error": error}
        evidence["calls"].append(entry)
        save()
        result = client.tool(name, arguments, error=error)
        entry["result"] = result
        save()
        return result
    def state(): return call("routing_state")
    def start_c(error=False): return call("routing_start", {"users": ["Owned recovery"], "allowed_pids": [f.c.pid], "mode": "concurrent", "multi_keyboard": True}, error)
    def focus(index):
        x, y = f.points[index]
        call("routing_input", {"action": "move_to", "user": 0, "x": x, "y": y})
        for down in (True, False): call("routing_input", {"action": "button", "user": 0, "button": "primary", "pressed": down})
    try:
        client = helper.Client(image, args.runtime.resolve(), folder / "mcp.stderr.log")
        f.start(client)
        evidence["owned_pids"] = [f.a.pid, f.c.pid]
        call("routing_start", {"users": ["Owned recovery"], "allowed_pids": [f.a.pid, f.c.pid], "mode": "concurrent", "multi_keyboard": True})
        live = helper.eventually(state, lambda s: s["running"] and len(s["users"]) == 1 and s["users"][0]["overlay"]["hwnd"])
        f.overlays.add(live["users"][0]["overlay"]["hwnd"])
        assert all(helper.user32.IsWindow(h) for h in f.overlays)
        focus(0)
        call("routing_input", {"action": "key", "user": 0, "key": "f6", "pressed": True})
        held = helper.eventually(lambda: read(f.a_report), lambda s: s["held_app_keys"] == [117] and len(s["key_events"]) == 1)
        evidence["groups"]["held_original_only"] = {"native": held, "state": state()}
        evidence["blocked_pump"] = f.pause_pump("none")
        begin = time.monotonic()
        evidence["stop_error"] = call("routing_stop", error=True)
        evidence["failed_stop_seconds"] = time.monotonic() - begin
        blocked = helper.eventually(state, lambda s: (s.get("recovery") or {}).get("phase") == "quarantined")
        quarantine(blocked["recovery"], f.a.pid, f.c.pid)
        assert f.a.poll() is None and read(f.a_report)["held_app_keys"] == [117]
        start_c(error=True)
        evidence["late_plain_release"] = f.late_release()
        still = state()
        quarantine(still["recovery"], f.a.pid, f.c.pid)
        start_c(error=True)
        assert not read(f.c_report)["mouse_events"] and not read(f.c_report)["key_events"]
        evidence["groups"]["quarantine_survives_late_delivery"] = {"failed": blocked, "after_late_up": still}
        evidence["original_exit"] = f.close_a()
        cleared = helper.eventually(state, lambda s: s["recovery"]["can_start"])
        assert not cleared["running"] and all(t["status"] == "exited" for t in cleared["recovery"]["targets"])
        start_c()
        new = helper.eventually(state, lambda s: s["running"] and len(s["users"]) == 1 and s["users"][0]["overlay"]["hwnd"])
        f.overlays.add(new["users"][0]["overlay"]["hwnd"])
        focus(1)
        for down in (True, False): call("routing_input", {"action": "key", "user": 0, "key": "f6", "pressed": down})
        fresh = helper.eventually(lambda: read(f.c_report), lambda s: len(s["key_events"]) >= 2)
        assert [(e["vk"], e["pressed"]) for e in fresh["key_events"]] == [(117, True), (117, False)]
        stopped = call("routing_stop")
        assert not stopped["running"] and stopped["recovery"]["can_start"]
        helper.eventually(lambda: all(not helper.user32.IsWindow(h) for h in f.overlays), bool)
        evidence["groups"]["explicit_fresh_start_and_stop"] = {"cleared": cleared, "native": fresh, "stop": stopped}
        evidence["mutation_calls"] = sum(c["name"] != "routing_state" for c in evidence["calls"])
        assert evidence["mutation_calls"] == 15
        evidence["result"] = "PENDING INDEPENDENT DATA REVIEW"
    except Exception as error:
        evidence["failure"] = repr(error)
        raise
    finally:
        errors = []
        if client:
            try: client.close()
            except Exception as error:
                errors.append(repr(error))
                if client.process.poll() is None: client.process.kill(); client.process.wait(timeout=12)
                client.stderr.close()
        errors += f.close()
        evidence.update(cleanup_errors=errors, owned_processes_closed=all(p.poll() is not None for p in f.owned) and (not client or client.process.poll() is not None),
                        known_overlays_closed=all(not helper.user32.IsWindow(h) for h in f.overlays))
        if errors: evidence["result"] = "NOT ACCEPTED"
        save()
        print(evidence["result"], folder, flush=True)


def gui(args):
    lifecycle = module("recovery_lifecycle_ui", "test-app-lifecycle-ui.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    g = Base(args, "failed-cleanup-recovery", offline=False, count=0)
    f = Fixtures(g.run, g.images[0], args.runtime.resolve())
    def recovery(): return ui.read_json(g.run / "ui.recovery.json")
    def still_quarantined(): return quarantine(recovery(), f.a.pid, f.c.pid)
    def frame(name):
        shot = g.screenshot(name)
        if not shot: return False
        g.evidence.setdefault("originals", {})[name] = shot
        return True
    try:
        discovery = ui.Client(g.images[0], args.runtime.resolve(), g.run / "discovery.stderr.log")
        try: f.start(discovery)
        finally: discovery.close()
        g.owned.extend(f.owned)
        g.evidence.update(producer=str(args.producer), producer_sha256=digest(args.producer), owned_pids=[f.a.pid, f.c.pid])
        def profile(index):
            return {"version": 1, "concurrent": True, "desktop": {"mode": "concurrent", "multi_keyboard": True,
                    "allowed_pids": [f.a.pid, f.c.pid] if index == 0 else [f.c.pid]},
                    "seats": [{"id": "owned-recovery", "name": "Owned recovery", "mouse": "virtual://mouse/1",
                               "keyboard": "virtual://keyboard/1", "desktop_start": f.points[index]}]}
        first, second = profile(0), profile(1)
        ui.put(g.run / "fresh-profile.json", second)
        prefs = {"version": 1, "appearance_mode": "dark", "zoom_percent": 100, "launch_minimized": False,
                 "minimize_to_tray": False, "close_to_tray": False, "launch_at_login": False, "shortcuts": []}
        steps = [ui.step("start", "Start routing"), ui.step("settings", "Settings"), ui.step("tools", "Settings category Tools"),
                 ui.step("lab", "Input lab"), ui.step("focus", "1 Hold click"), ui.step("up", "1 Release click"),
                 ui.step("hold", "Hold key", {"selected_user": 0, "key_name": "f6"}), ui.step("home", "Back to Home"),
                 ui.step("failed-stop", "Stop routing"), ui.step("details", "Recovery details"), ui.step("check", "Check recovery"),
                 ui.step("dismiss", "Close recovery"), ui.step("blocked-start", "Start routing", disabled=True),
                 ui.step("settings-new", "Settings"), ui.step("users-category", "Settings category Users & devices"),
                 ui.step("profiles", "Profiles & backend"), ui.step("load-new", "Load profile", {"profile_path": g.run / "fresh-profile.json"}),
                 ui.step("home-new", "Back to Home"), ui.step("still-blocked", "Start routing", disabled=True),
                 ui.step("fresh-start", "Start routing"), ui.step("fresh-settings", "Settings"), ui.step("fresh-tools", "Settings category Tools"),
                 ui.step("fresh-lab", "Input lab"), ui.step("fresh-hold", "1 Hold click"), ui.step("fresh-home", "Back to Home"),
                 ui.step("fresh-stop", "Stop routing")]
        assert len(steps) == 26
        def verify(name, sample):
            assert ui.read_json(g.preferences_path) == prefs
            if name in ("start", "fresh-start"):
                if len(sample["state"]) != 1 or not sample["state"][0]["overlay"]["hwnd"]: return False
                hwnd = sample["state"][0]["overlay"]["hwnd"]
                assert ui.user32.IsWindow(hwnd)
                f.overlays.add(hwnd)
            if name == "hold" and read(f.a_report)["held_app_keys"] != [117]: return False
            if name == "home":
                g.once("block-owned-pump", lambda: f.pause_pump("peek_no_remove"))
            if name == "failed-stop":
                if recovery()["phase"] != "quarantined": return False
                still_quarantined()
                assert f.a.poll() is None
                g.once("recovery-size", lambda: geometry.resize_owned_client(g.main_hwnd(), [360, 360]))
            if name in ("details", "check", "dismiss", "blocked-start", "settings-new", "users-category", "profiles", "load-new", "home-new", "still-blocked"):
                sample["recovery"] = still_quarantined()
            if name == "check":
                sample["late_native_up"] = g.once("late-plain-release", f.late_release)
                if not frame("routing-recovery-details"): return False
            if name == "load-new":
                if sample["profile_state"]["profile"]["desktop"]["allowed_pids"] != [f.c.pid]: return False
                assert not read(f.c_report)["mouse_events"] and not read(f.c_report)["key_events"]
            if name == "still-blocked":
                if not frame("start-unavailable-after-profile-change"): return False
                sample["original_exit"] = g.once("close-exact-original", f.close_a)
                current = ui.eventually(recovery, lambda s: s["can_start"])
                sample["after_original_exit"] = current
                assert not any(t["status"] != "exited" for t in current["targets"])
            if name == "fresh-hold" and not read(f.c_report)["buttons"] & 1: return False
            if name == "fresh-stop":
                native = read(f.c_report)
                if native["buttons"] or len(native["mouse_events"]) < 2: return False
                assert [(e["button"], e["pressed"]) for e in native["mouse_events"]] == [(0, True), (0, False)]
                if any(ui.user32.IsWindow(h) for h in f.overlays) or not recovery()["can_start"]: return False
                sample["fresh_native_plain_pair"] = native
            return True
        def after(gate):
            gate.expected_exit = True
            assert geometry.user32.PostMessageW(gate.main_hwnd(), 0x10, 0, 0)
            gate.gui.wait(timeout=10)
            assert gate.gui.returncode == 0 and not hotkeys.lifecycle_window(gate.gui.pid)
            return True
        source = g.run / "runner-source"
        source.mkdir(exist_ok=True)
        shutil.copy2(Path(__file__), source / Path(__file__).name)
        shutil.copy2(Path(__file__).with_name("test-output-shutdown-timeout.py"), source / "test-output-shutdown-timeout.py")
        g.execute(first, prefs, steps, verify, after_controls=after)
        g.evidence["result"] = "PENDING INDEPENDENT DATA/PIXEL REVIEW"
    finally:
        g.evidence["fixture_cleanup_errors"] = f.close()
        g.close()
        lifecycle.remove_startup_key(g.startup_key)
        g.evidence["known_overlays_closed"] = all(not ui.user32.IsWindow(h) for h in f.overlays)
        if g.evidence["fixture_cleanup_errors"] or not g.evidence["known_overlays_closed"]:
            g.evidence["result"] = "NOT ACCEPTED"
        g.save()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready", action="store_true")
    parser.add_argument("--phase", choices=("standalone", "gui"), required=True)
    for name in ("executable", "x86-executable", "runtime", "producer"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--producer-sha256", required=True)
    args = parser.parse_args()
    freeze(args)
    standalone(args) if args.phase == "standalone" else gui(args)


if __name__ == "__main__":
    main()
