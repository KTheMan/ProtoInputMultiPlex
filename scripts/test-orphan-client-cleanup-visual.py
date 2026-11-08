"""Focused real Agent Access reveal correction; no two-client data replay."""
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
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def digest(path): return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def freeze(args):
    assert args.ready, "Await source/producer/ROOT READY"
    assert digest(args.producer) == args.producer_sha256.lower()
    repo = Path(__file__).resolve().parent.parent
    prior = repo / "runtime/acceptance-graph-orphan-client-cleanup-20261015-053751"
    assert args.from_run.resolve() == prior
    assert digest(prior / "evidence.json") == "35e437f74d8d2a391aebc07b06cd7138c462c4d40ae9802a68e04cd29cbaec69"
    assert digest(prior / "independent-review.json") == "6c8f25872b2a9d0551368a8009007f66aba89ab7b74dfa0d48114e9793fbf4e5"
    p = json.loads(args.producer.read_text(encoding="utf-8"))
    old_path = repo / "runtime/producer-orphan-client-cleanup-20261015-053312/verification.json"
    assert digest(old_path) == "0407e61bc7b04efd0516b4baa2a2ab60653dc37a534b268f6a797ea79cfa058c"
    old = json.loads(old_path.read_text(encoding="utf-8"))
    assert p["result"] == "PASS" and p["runtime_sha256"] == old["runtime_sha256"]
    assert set(p["source_after_sha256"]) == set(old["source_after_sha256"])
    changed = [n.replace("\\", "/") for n,h in p["source_after_sha256"].items() if old["source_after_sha256"][n] != h]
    assert set(changed) == {"crates/protoinputmultiplex/src/gui.rs", "crates/protoinputmultiplex/src/agent_ui.rs"}, changed
    assert all(digest(repo/n) == h for n,h in p["source_after_sha256"].items())
    assert digest(args.executable) == p["build_sha256"]["x64"] and digest(args.x86_executable) == p["build_sha256"]["i686"]
    assert all(digest(args.runtime/n) == h for n,h in p["runtime_sha256"].items())


def workflow(args):
    lifecycle = module("orphan_visual_lifecycle", "test-app-lifecycle-ui.py")
    transport = module("orphan_visual_pipe", "test-connected-agents-acceptance.py")
    actions = module("orphan_visual_actions", "test-connected-macro-studio-acceptance.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    class Gate(Base):
        def __init__(self):
            super().__init__(args, "orphan-client-cleanup-visual", offline=False, count=1)
            self.client, self.stage, self.requests, self.layers = None, 0, [], set()
        def probes(self): return ui.eventually(lambda:super(Gate,self).probes(),lambda value:True,3)
        def snapshot(self): return ui.eventually(lambda:super(Gate,self).snapshot(),lambda value:True,3)
        def request(self, kind="inspect", **fields):
            entry = {"kind":kind,"fields":fields}
            self.requests.append(entry)
            result = self.client.request(kind,**fields)
            entry["result"] = result
            assert not result.get("error"), result
            for event in result["events"]:
                if isinstance(event,dict) and "State" in event:
                    self.layers.update(u["overlay"]["hwnd"] for u in event["State"] if u.get("overlay"))
            self.evidence["requests"] = self.requests
            self.save()
            return result
        def setup_pending(self):
            if self.stage: return
            paths = list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{self.gui.pid}-*.json"))
            assert len(paths)==1
            self.descriptor = paths[0]
            self.client = transport.OwnedPipe(self.descriptor)
            initial = self.request()
            assert initial["policy"]["user_ids"] == ["visual-cleanup-a"] and initial["policy"]["allowed_pids"] == [self.target.pid]
            x,y = self.point
            for request in [{"MoveTo":{"user":0,"x":x,"y":y}},
                            {"Input":actions.button(0,True,0)["packet"]},{"Input":actions.button(0,False,0)["packet"]},
                            {"Input":actions.key(0,True,117,64)["packet"]}]:
                self.request("execute",request=request)
            native = ui.eventually(self.probes,lambda reports:reports[0]["held_keys"] == [117])[0]
            assert [(e["vk"],e["pressed"]) for e in native["key_events"]] == [(117,True)]
            assert len(self.layers)==1 and all(ui.user32.IsWindow(h) for h in self.layers)
            child = native["capture_hwnd"]
            geometry.user32.GetParent.argtypes=[wintypes.HWND]
            geometry.user32.GetParent.restype=wintypes.HWND
            assert geometry.user32.GetParent(child)==self.window["hwnd"]
            pid = wintypes.DWORD()
            assert geometry.user32.GetWindowThreadProcessId(child,ctypes.byref(pid)) and pid.value==self.target.pid
            geometry.user32.GetClassNameW.argtypes=[wintypes.HWND,wintypes.LPWSTR,ctypes.c_int]
            name=ctypes.create_unicode_buffer(32)
            assert geometry.user32.GetClassNameW(child,name,32) and name.value.casefold()=="edit"
            assert geometry.user32.PostMessageW(child,0x10,0,0)
            ui.eventually(lambda:not ui.user32.IsWindow(child),bool)
            assert self.target.poll() is None and ui.user32.IsWindow(self.window["hwnd"])
            self.evidence["closed_child"]={"hwnd":child,"pid":self.target.pid,"parent_alive":True}
            self.client.close()
            self.stage=1
        def agent(self): return ui.read_json(self.run/"ui.agent.json")
        def frame(self,name):
            # Async Screenshot can be requested before this state and delivered
            # afterward. Discard the first fresh write; never certify pixels by mtime.
            from PIL import Image
            import io
            current = self.agent()
            if name == "client-cleanup-pending":
                assert self.target.poll() is None
                assert any(row["client"] == self.client.client and row["pending_owners"] > 0
                           for row in current["cleanup"])
            else:
                assert self.target.poll() is not None and not current["cleanup"]
            assert self.client.client not in current["clients"]
            holds = self.evidence.setdefault("capture_holds", {})
            hold = holds.setdefault(name, {"started_ns": time.time_ns(), "writes": []})
            eligible_after_ns = hold["started_ns"] + 3_200_000_000
            hold["grace_until_ns"] = eligible_after_ns
            if time.time_ns() < eligible_after_ns:
                return False  # Nonblocking full-cadence grace; native state remains held.
            source = self.run / "ui.png"
            try:
                before = source.stat()
                if before.st_mtime_ns <= eligible_after_ns:
                    return False
                data = source.read_bytes()
                after = source.stat()
                if (before.st_mtime_ns, before.st_size) != (after.st_mtime_ns, after.st_size):
                    return False
                hwnd = self.main_hwnd()
                actual = geometry.geometry(hwnd)
                with Image.open(io.BytesIO(data)) as image:
                    image.load()
                    if list(image.size) != actual["client_pixels"]:
                        return False
                assert actual["client_pixels"] == [680, 620]
            except (OSError, ValueError):
                return False
            record = {"mtime_ns": before.st_mtime_ns, "size": before.st_size,
                      "sha256": hashlib.sha256(data).hexdigest()}
            if not any(write["mtime_ns"] == record["mtime_ns"] for write in hold["writes"]):
                hold["writes"].append(record)
                self.save()
            if len(hold["writes"]) < 2:
                return False  # First new image is observed only, never the final original.
            shot = geometry.copy_owned_product_frame(source, self.run / (name + ".png"), hwnd,
                                                      eligible_after_ns / 1_000_000_000)
            if not shot:
                return False
            # A concurrent rewrite is a future candidate, not a silently different copy.
            if shot["sha256"] != record["sha256"]:
                return False
            self.evidence.setdefault("originals", {})[name] = shot
            hold["accepted_write"] = record
            return True
    g=Gate()
    try:
        g.target=subprocess.Popen([str(g.images[0]),"probe","--automation","--report",str(g.run/"probe-0.json")],creationflags=subprocess.CREATE_NO_WINDOW)
        g.owned.append(g.target)
        ui.eventually(g.probes,lambda value:value[0]["frames"]>2)
        discovery=ui.Client(g.images[0],args.runtime.resolve(),g.run/"discovery.stderr.log")
        try:
            def window():return next((w for w in discovery.tool("routing_windows")["windows"] if w["pid"]==g.target.pid and w["title"].startswith("ProtoInputMultiPlex Automation Test")),None)
            g.window=ui.eventually(window,bool)
            assert ui.user32.SetWindowPos(g.window["hwnd"],wintypes.HWND(-1),30,40,420,360,0x10)
            g.window=ui.eventually(window,bool)
            g.point=[g.window["client"]["left"]+120,g.window["client"]["top"]+100]
        finally:discovery.close()
        profile={"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":[g.target.pid]},
                 "seats":[{"id":"visual-cleanup-a","name":"Owned visual cleanup","mouse":"virtual://mouse/1","keyboard":"virtual://keyboard/1","desktop_start":g.point,"cursor":{"lock_window":False}}]}
        prefs={"version":1,"appearance_mode":"dark","zoom_percent":100,"launch_minimized":False,"minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,"shortcuts":[]}
        steps=[ui.step("start","Start routing"),ui.step("settings","Settings"),ui.step("category","Settings category Agent access"),ui.step("agent","Agent Access"),
               ui.step("enable","Enable agent access",{"agent_users":"visual-cleanup-a","agent_pids":g.target.pid,"agent_session_control":"false","agent_capture":"false","agent_app_preferences":"false"}),
               ui.step("error","Last release error"),ui.step("home","Back to Home"),ui.step("stop","Stop routing")]
        expected=None
        def verify(name,sample):
            nonlocal expected
            actual=sample["profile_state"]["profile"]
            if expected is None:expected=copy.deepcopy(actual)
            assert actual==expected and ui.read_json(g.preferences_path)==prefs
            if name=="agent":g.once("normal-agent-size",lambda:geometry.resize_owned_client(g.main_hwnd(),[680,620]))
            if name=="enable":
                g.setup_pending()
                state=g.agent()
                if g.client.client in state["clients"] or not any(r["client"]==g.client.client and r["pending_owners"]>0 for r in state["cleanup"]):return False
                sample["pending_state"]=state
            if name=="error":
                if g.stage==1:
                    state=g.agent()
                    assert state["cleanup"] and g.target.poll() is None
                    if not g.frame("client-cleanup-pending"):return False
                    g.evidence["pending_state"]=state
                    assert geometry.user32.PostMessageW(g.window["hwnd"],0x10,0,0)
                    g.target.wait(timeout=8)
                    assert g.target.returncode==0
                    g.stage=2
                if g.agent()["cleanup"]:return False
                if not g.frame("client-cleanup-recovered"):return False
                g.evidence["recovered_state"]=g.agent()
                assert sum(r["kind"]=="execute" for r in g.requests)==4
                g.stage=3
            if name=="stop" and (any(ui.user32.IsWindow(h)for h in g.layers) or g.descriptor.exists()):return False
            return True
        def after(gate):
            gate.expected_exit=True
            assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0)
            gate.gui.wait(timeout=10)
            assert gate.gui.returncode==0 and not hotkeys.lifecycle_window(gate.gui.pid)
            return True
        source=g.run/"runner-source";source.mkdir()
        for name in(Path(__file__).name,"test-connected-agents-acceptance.py","test-connected-macro-studio-acceptance.py"):
            shutil.copy2(Path(__file__).with_name(name),source/name)
        g.evidence.update(producer=str(args.producer),producer_sha256=digest(args.producer),retained_data_run=str(args.from_run),scope="One-client visualtail only; previous two-client data retained")
        g.execute(profile,prefs,steps,verify,after_controls=after)
        g.evidence["result"]="PENDING INDEPENDENT VISUAL REVIEW"
    finally:
        if g.client:g.client.close()
        g.close();lifecycle.remove_startup_key(g.startup_key)
        g.evidence["raw_handle_closed"]=not g.client or g.client.handle is None
        g.evidence["known_overlays_closed"]=all(not ui.user32.IsWindow(h)for h in g.layers)
        g.save()


def main():
    parser=argparse.ArgumentParser();parser.add_argument("--ready",action="store_true")
    for name in("executable","x86-executable","runtime","producer","from-run"):parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--producer-sha256",required=True)
    args=parser.parse_args();freeze(args);workflow(args)


if __name__=="__main__":main()
