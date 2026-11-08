"""One retired-client EOF/process-exit integration gate. Source-only until READY."""
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
import tempfile
import time


def module(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def digest(path): return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def freeze(args):
    assert args.ready, "Await frozen source/producer review and ROOT READY"
    assert digest(args.producer) == args.producer_sha256.lower()
    p = json.loads(args.producer.read_text(encoding="utf-8"))
    assert p["result"] == "PASS"
    repo = Path(__file__).resolve().parent.parent
    assert all(digest(repo / n) == h for n, h in p["source_after_sha256"].items())
    assert digest(args.executable) == p["build_sha256"]["x64"]
    assert digest(args.x86_executable) == p["build_sha256"]["i686"]
    assert all(digest(args.runtime / n) == h for n, h in p["runtime_sha256"].items())


# Final public state-field binding must be reviewed after source freeze.
CLEANUP_FIELD = "cleanup"


def workflow(args):
    lifecycle = module("orphan_lifecycle", "test-app-lifecycle-ui.py")
    transport = module("orphan_transport", "test-connected-agents-acceptance.py")
    actions = module("orphan_actions", "test-connected-macro-studio-acceptance.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    ids = ["orphan-owned-a", "orphan-owned-b"]

    class Gate(Base):
        def __init__(self):
            super().__init__(args, "orphan-client-cleanup", offline=False, count=2)
            self.raw_clients, self.layers = [], set()
            self.requests, self.frames, self.stage = [], {}, 0
            self.profile_expected = None
            self.mutations = 0

        def snapshot(self): return ui.eventually(lambda: super(Gate, self).snapshot(), lambda value: True, 3)
        def probes(self): return ui.eventually(lambda: super(Gate, self).probes(), lambda value: True, 3)

        def fixture(self):
            for i in range(2):
                self.owned.append(subprocess.Popen([str(self.images[0]), "probe", "--automation", "--report", str(self.run / f"probe-{i}.json")], creationflags=subprocess.CREATE_NO_WINDOW))
            self.targets = self.owned[:2]
            self.pids = [p.pid for p in self.targets]
            discovery = ui.Client(self.images[0], args.runtime.resolve(), self.run / "discovery.stderr.log")
            try:
                def inventory():
                    rows = discovery.tool("routing_windows")["windows"]
                    return [next((w for w in rows if w["pid"] == p and w["title"].startswith("ProtoInputMultiPlex Automation Test")), None) for p in self.pids]
                self.windows = ui.eventually(inventory, lambda value: all(value))
                for i, w in enumerate(self.windows):
                    assert ui.user32.SetWindowPos(w["hwnd"], wintypes.HWND(-1), 30 + i * 460, 40, 420, 360, 0x10)
                self.windows = ui.eventually(inventory, lambda value: all(value))
                self.points = [[w["client"]["left"] + 120, w["client"]["top"] + 100] for w in self.windows]
            finally: discovery.close()

        def request(self, client, kind="inspect", **fields):
            entry = {"client": client.client, "kind": kind, "fields": fields, "at_unix": time.time()}
            self.requests.append(entry)
            if kind == "execute": self.mutations += 1
            self.evidence["requests"] = self.requests
            self.save()
            result = client.request(kind, **fields)
            entry["result"] = result
            assert not result.get("error"), result
            if self.profile_expected is not None:
                assert result["profile"] == self.profile_expected, "Cleanup changed GUI profile"
            for event in result["events"]:
                if isinstance(event, dict) and "State" in event:
                    self.layers.update(u["overlay"]["hwnd"] for u in event["State"] if u.get("overlay"))
            self.save()
            return result

        def execute_request(self, client, request): return self.request(client, "execute", request=request)

        def key(self, client, user, vk, down):
            packet = actions.key(user, down, vk, {116:63, 117:64, 118:65, 119:66}[vk])["packet"]
            return self.execute_request(client, {"Input": packet})

        def select(self, user):
            x, y = self.points[user]
            self.execute_request(self.x, {"MoveTo": {"user": user, "x": x, "y": y}})
            for down in (True, False): self.execute_request(self.x, {"Input": actions.button(user, down, 0)["packet"]})

        def rows(self, result):
            assert CLEANUP_FIELD in result, "Bind frozen cleanup snapshot field before READY"
            return result[CLEANUP_FIELD]

        def connect_and_hold(self):
            paths = list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{self.gui.pid}-*.json"))
            assert len(paths) == 1
            self.descriptor = paths[0]
            self.x = transport.OwnedPipe(self.descriptor)
            self.raw_clients.append(self.x)
            self.y = transport.OwnedPipe(self.descriptor)
            self.raw_clients.append(self.y)
            assert self.x.client != self.y.client
            initial = self.request(self.y)
            assert set(initial["policy"]["user_ids"]) == set(ids)
            assert set(initial["policy"]["allowed_pids"]) == set(self.pids)
            assert initial["clients"] == sorted([self.x.client, self.y.client]) and not self.rows(initial)
            self.profile_expected = copy.deepcopy(initial["profile"])
            self.select(0)
            self.select(1)
            self.key(self.x, 0, 117, True)
            self.key(self.x, 1, 119, True)
            self.key(self.x, 1, 118, True)
            self.key(self.y, 1, 118, True)
            native = ui.eventually(self.probes, lambda r: r[0]["held_keys"] == [117] and set(r[1]["held_keys"]) == {118,119})
            assert [(e["vk"],e["pressed"]) for e in native[0]["key_events"]] == [(117,True)]
            assert [(e["vk"],e["pressed"]) for e in native[1]["key_events"]] == [(119,True),(118,True)]
            assert len(self.layers) == 2 and all(ui.user32.IsWindow(h) for h in self.layers)
            self.evidence["initial_native"] = native
            child = native[0]["capture_hwnd"]
            geometry.user32.GetParent.argtypes = [wintypes.HWND]
            geometry.user32.GetParent.restype = wintypes.HWND
            assert geometry.user32.GetParent(child) == self.windows[0]["hwnd"]
            owner = wintypes.DWORD()
            assert geometry.user32.GetWindowThreadProcessId(child, ctypes.byref(owner)) and owner.value == self.pids[0]
            name = ctypes.create_unicode_buffer(64)
            geometry.user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
            assert geometry.user32.GetClassNameW(child, name, 64)
            assert name.value.casefold() == "edit"
            assert geometry.user32.PostMessageW(child, 0x10, 0, 0)
            ui.eventually(lambda: not ui.user32.IsWindow(child), bool)
            assert self.targets[0].poll() is None and ui.user32.IsWindow(self.windows[0]["hwnd"])
            self.evidence["closed_child_only"] = {"hwnd": child, "pid": self.pids[0], "parent_alive": True}
            self.x.close()  # Actual EOF, no Disconnect request or identity reuse.
            self.evidence["retired_client"] = self.x.client

        def expected_b(self, pairs, released=False):
            result = [(119,True),(118,True),(119,False)] + [(116,d) for _ in range(pairs) for d in (True,False)]
            if released: result.append((118,False))
            return result

        def assert_b(self, pairs, released=False):
            report = self.probes()[1]
            assert [(e["vk"],e["pressed"]) for e in report["key_events"]] == self.expected_b(pairs,released), report
            assert set(report["held_keys"]) == (set() if released else {118}), report
            return report

        def pair(self):
            for down in (True,False): self.key(self.y,1,116,down)

        def frame(self, name):
            shot = self.screenshot(name)
            if not shot: return False
            self.frames[name] = shot
            self.evidence["originals"] = self.frames
            return True

        def native_case(self, pending_only=False):
            if self.stage == 0:
                self.connect_and_hold()
                self.stage = 1
            if self.stage == 1:
                def pending(result):
                    rows = self.rows(result)
                    return self.x.client not in result["clients"] and any(row["client"] == self.x.client and row["pending_owners"] > 0 and row["attempts"] >= 2 for row in rows)
                current = ui.eventually(lambda:self.request(self.y),pending)
                self.evidence["pending_snapshot"] = current
                ui.eventually(self.probes,lambda r:len(r[1]["key_events"]) >= 3)
                self.assert_b(0)
                self.pair()
                ui.eventually(self.probes,lambda r:len(r[1]["key_events"]) >= 5)
                self.assert_b(1)
                self.stage = 2
            if self.stage == 2 and pending_only:
                return True
            if self.stage == 2:
                agent = ui.read_json(self.run / "ui.agent.json")
                if not any(row["client"] == self.x.client for row in self.rows(agent)): return False
                if not self.frame("retired-client-pending"): return False
                assert self.targets[0].poll() is None
                assert geometry.user32.PostMessageW(self.windows[0]["hwnd"],0x10,0,0)
                self.targets[0].wait(timeout=8)
                assert self.targets[0].returncode == 0
                self.stage = 3
            if self.stage == 3:
                recovered = ui.eventually(lambda:self.request(self.y),lambda result:not self.rows(result))
                assert self.x.client not in recovered["clients"] and self.y.client in recovered["clients"]
                self.assert_b(1)
                self.pair()
                ui.eventually(self.probes,lambda r:len(r[1]["key_events"]) >= 7)
                self.assert_b(2)
                self.evidence["recovered_snapshot"] = recovered
                self.stage = 4
            if self.stage == 4:
                if self.rows(ui.read_json(self.run / "ui.agent.json")): return False
                if not self.frame("retired-client-recovered"): return False
                self.key(self.y,1,118,False)
                ui.eventually(self.probes,lambda r:len(r[1]["key_events"]) >= 8)
                self.evidence["final_native_b"] = self.assert_b(2,True)
                assert self.mutations == 15
                self.evidence["raw_mutation_calls"] = self.mutations
                self.stage = 5
            return True

    g = Gate()
    try:
        g.fixture()
        profile = {"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":g.pids},
                   "seats":[{"id":ids[i],"name":"Owned cleanup " + ("Alpha" if i==0 else "Beta"),"mouse":f"virtual://mouse/{i+1}",
                             "keyboard":f"virtual://keyboard/{i+1}","desktop_start":g.points[i],"cursor":{"lock_window":False}} for i in range(2)]}
        prefs = {"version":1,"appearance_mode":"dark","zoom_percent":100,"launch_minimized":False,"minimize_to_tray":False,
                 "close_to_tray":False,"launch_at_login":False,"shortcuts":[]}
        steps = [ui.step("start","Start routing"),ui.step("settings","Settings"),ui.step("agent-category","Settings category Agent access"),
                 ui.step("agent","Agent Access"),ui.step("enable","Enable agent access",{"agent_users":','.join(ids),"agent_pids":','.join(map(str,g.pids)),
                     "agent_session_control":"false","agent_capture":"false","agent_app_preferences":"false"}),
                 ui.step("error-details","Last release error"),ui.step("home","Back to Home"),ui.step("stop","Stop routing")]
        def verify(name,sample):
            assert ui.read_json(g.preferences_path) == prefs
            if name == "enable": return g.native_case(pending_only=True)
            if name == "error-details": return g.native_case()
            if name == "stop":
                if any(ui.user32.IsWindow(h) for h in g.layers) or g.descriptor.exists(): return False
                g.assert_b(2,True)
            return True
        def after(gate):
            gate.expected_exit = True
            assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0)
            gate.gui.wait(timeout=10)
            assert gate.gui.returncode == 0 and not hotkeys.lifecycle_window(gate.gui.pid)
            return True
        source = g.run / "runner-source"
        source.mkdir()
        for name in (Path(__file__).name,"test-connected-agents-acceptance.py","test-connected-macro-studio-acceptance.py"):
            shutil.copy2(Path(__file__).with_name(name),source/name)
        g.evidence.update(producer=str(args.producer),producer_sha256=digest(args.producer),owned_pids=g.pids)
        g.execute(profile,prefs,steps,verify,after_controls=after)
        g.evidence["result"] = "PENDING INDEPENDENT DATA/PIXEL REVIEW"
    finally:
        for client in g.raw_clients: client.close()
        g.evidence["raw_handles_closed"] = all(c.handle is None for c in g.raw_clients)
        g.close()
        lifecycle.remove_startup_key(g.startup_key)
        g.evidence["known_overlays_closed"] = all(not ui.user32.IsWindow(h) for h in g.layers)
        g.save()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready",action="store_true")
    for name in ("executable","x86-executable","runtime","producer"):
        parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--producer-sha256",required=True)
    args = parser.parse_args()
    freeze(args)
    workflow(args)


if __name__ == "__main__": main()
