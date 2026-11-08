"""Focused replica-only responsive-layout continuation; frozen READY required."""
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


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def workflow(args):
    lifecycle = module("utility_lifecycle", "test-app-lifecycle-ui.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    ids = ["utility-a", "utility-b"]
    options = {"position":"anchored","offset_x":0,"offset_y":0,"motion":False,
        "wheel":False,"horizontal_wheel":False,"buttons":[False,False,False,True,False],
        "click_delay_ms":0,"step_interval_ms":0}
    graph = {"version":1,"edges":[{"source":ids[0],"target":ids[1],"mouse":True,
        "keyboard":False,"text":False,"options":copy.deepcopy(options)}]}
    assert args.from_run
    assert args.from_run.name=="acceptance-graph-productivity-ux-20261015-013410"
    assert hashlib.sha256((args.from_run/"evidence.json").read_bytes()).hexdigest()=="e2db3f0d91eb6783575a55001c302500fde40eafa55707506dc0386a83a79a0a"
    prefix = ui.read_json(args.from_run/"evidence.json")
    review = ui.read_json(args.from_run/"independent-visual-review.json")
    assert prefix["result"]=="PENDING INDEPENDENT VISUAL REVIEW" and prefix["control_state_result"]=="PASS"
    assert len(prefix["samples"])==18 and review["decision"]=="NOT ACCEPTED"
    assert prefix["owned_processes_closed"] and prefix["connected_mcp_closed"] and prefix["raw_handles_closed"]

    class Gate(Base):
        def __init__(self):
            super().__init__(args,"productivity-vu-ux",offline=False,count=3)
            self.layers = set()
            self.expected_profile = None
            self.frames, self.frame_started, self.checks = {}, {}, {}

        def probes(self):
            return ui.eventually(lambda:super(Gate,self).probes(),lambda value:True,3)

        def snapshot(self):
            return ui.eventually(lambda:super(Gate,self).snapshot(),lambda value:True,3)

        def fixture(self):
            geometry.physical_pixel_context()
            for i in range(3):
                self.owned.append(subprocess.Popen([str(self.images[0]),"probe","--automation",
                    "--report",str(self.run/f"probe-{i}.json")],creationflags=subprocess.CREATE_NO_WINDOW))
            self.pids = [process.pid for process in self.owned]
            def windows():
                found = {}
                @geometry.callback_type
                def visit(hwnd,unused):
                    pid = wintypes.DWORD()
                    geometry.user32.GetWindowThreadProcessId(hwnd,ctypes.byref(pid))
                    if pid.value in self.pids and not geometry.user32.GetWindow(hwnd,4):
                        title = ctypes.create_unicode_buffer(80)
                        geometry.user32.GetWindowTextW(hwnd,title,len(title))
                        if title.value.startswith("ProtoInputMultiPlex Automation Test"):
                            found[pid.value] = int(hwnd)
                    return True
                assert geometry.user32.EnumWindows(visit,0)
                return found
            found = ui.eventually(windows,lambda value:len(value)==3,20)
            geometry.user32.ClientToScreen.argtypes = [wintypes.HWND,ctypes.POINTER(wintypes.POINT)]
            for i,pid in enumerate(self.pids):
                hwnd = found[pid]
                assert geometry.user32.SetWindowPos(hwnd,wintypes.HWND(-1),30+460*i,40,420,360,0x10)
                point = wintypes.POINT(120,100)
                assert geometry.user32.ClientToScreen(hwnd,ctypes.byref(point))
                self.points.append([point.x,point.y])

        def record(self,name,value):
            assert name not in self.checks
            self.checks[name] = value
            self.evidence["checks"] = self.checks
            self.save()

        def frame(self,name,size):
            if name in self.frames:return True
            hwnd = self.main_hwnd()
            if name not in self.frame_started:
                geometry.resize_owned_client(hwnd,size);self.frame_started[name] = time.time();return False
            current = geometry.geometry(hwnd)
            if any(abs(a-b)>2 for a,b in zip(current["client_logical"],size)):return False
            shot = geometry.copy_owned_product_frame(self.run/"ui.png",self.run/(name+".png"),hwnd,self.frame_started[name])
            if not shot:return False
            shot["geometry"] = current;self.frames[name] = shot
            return True

        def gui_roster(self):
            return ui.eventually(lambda:ui.read_json(self.run/"ui.virtual-users.json"),lambda v:True,3)

        def project(self,expected):
            previous = self.expected_profile
            def gui_profile():
                profile = self.snapshot()["profile"]
                assert profile==previous or profile==expected,"GUI profile suffered unrelated mutation"
                return profile
            ui.eventually(gui_profile,lambda p:p==expected,10)
            self.expected_profile = expected

        def revealed_frame(self,name,next_label):
            # Resize first, then let the normal next-control observer reveal
            # its real header/action. This does not synthesize wheel input.
            if name not in self.frame_started:return self.frame(name,[320,360])
            progress = ui.read_json(self.run/"ui.progress.json")
            if progress.get("response_label")!=next_label or not progress.get("response_visible"):return False
            if time.time()-self.frame_started[name]<.5:return False
            return self.frame(name,[320,360])

    g = Gate()
    done = set()
    values = {}
    prefs = {"version":1,"appearance_mode":"dark","zoom_percent":100,"launch_minimized":False,
        "minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,"shortcuts":[]}

    def verify(name,sample):
        g.layers.update(u["overlay"]["hwnd"] for u in sample["state"] if u.get("overlay"))
        if g.expected_profile is not None and name not in {"apply-graph","start-clone","stop-clone"}:
            assert g.snapshot()["profile"]==g.expected_profile
        if name=="start":
            if len(sample["state"])!=2:return False
            if name not in done:
                g.expected_profile=copy.deepcopy(g.snapshot()["profile"])
                values["preferences_bytes"]=g.preferences_path.read_bytes()
                g.layers.update(u["overlay"]["hwnd"] for u in sample["state"] if u.get("overlay"))
                done.add(name)
        if name=="start-clone":
            current = g.gui_roster()
            if not current["state"] or current["state"]["revision"]!=1 or current["pending"]:return False
            if name not in done:
                slot = current["state"]["slots"][-1]
                assert slot["user"]==2 and slot["active"] and slot["clone_of"]==ids[0]
                seat = slot["seat"]
                expected = copy.deepcopy(g.expected_profile)
                clone = copy.deepcopy(expected["seats"][0])
                clone.update(id=seat["id"],name="Owned utility replica",mouse=seat["mouse"],
                    keyboard=seat["keyboard"],target_pid=g.pids[2],desktop_start=None)
                assert seat==clone and seat["id"] not in ids
                expected["seats"].append(clone)
                expected["replication"]["edges"].append({"source":ids[0],"target":seat["id"],
                    "mouse":True,"keyboard":True,"text":True,"options":copy.deepcopy(options)})
                g.project(expected)
                values["clone"] = slot
                g.record("local_clone_ack",{"state":current["state"]})
                done.add(name)
        if name=="select-clone":
            current = g.gui_roster()
            if current["selected_clone_id"]!=values["clone"]["seat"]["id"]:return False
            assert not current["pending"] and current["state"]["slots"][2]["active"]
            if not g.frame("replica-active-normal",[760,620]):return False
        if name=="stop-clone":
            current = g.gui_roster()
            if not current["state"] or current["state"]["revision"]!=2 or current["pending"]:return False
            assert not current["state"]["slots"][2]["active"]
            if name not in done:
                expected = copy.deepcopy(g.expected_profile);expected["replication"] = copy.deepcopy(graph)
                g.project(expected);done.add(name)
            if not g.revealed_frame("replica-selected-tombstone-minimum","Stable identity details"):return False
        if name=="replica-details":
            current = g.gui_roster()
            assert current["selected_clone_id"]==values["clone"]["seat"]["id"]
            assert current["state"]["revision"]==2 and not current["state"]["slots"][2]["active"] and not current["pending"]
            if not g.revealed_frame("replica-identities-bottom-minimum","Stop selected virtual replica"):return False
        if name=="stopped-action":
            current = g.gui_roster()
            assert current["state"]["revision"]==2 and not current["state"]["slots"][2]["active"]
            assert not current["pending"] and sample["event"]["observed_disabled"] and not sample["event"]["clicked"]
        return True

    def after(gate):
        assert gate.snapshot()["profile"]==gate.expected_profile
        assert gate.preferences_path.read_bytes()==values["preferences_bytes"]
        assert not any(r["held_keys"] or r["buttons"] for r in gate.probes())
        ui.put(gate.run/"final-profile-before-close.json",gate.snapshot()["profile"])
        gate.expected_exit = True
        assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0)
        gate.gui.wait(timeout=15)
        assert gate.gui.returncode==0 and not hotkeys.lifecycle_window(gate.gui.pid)
        assert all(not ui.user32.IsWindow(hwnd) for hwnd in gate.layers)
        assert not list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{gate.gui.pid}-*.json"))
        return True

    try:
        g.fixture()
        profile = {"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,
            "allowed_pids":g.pids},"replication":{"version":1,"edges":[]},"seats":[{"id":ids[i],
                "name":"Owned utility "+("Alpha" if i==0 else "Beta"),"mouse":f"virtual://mouse/{i+1}",
                "keyboard":f"virtual://keyboard/{i+1}","desktop_start":g.points[i],"cursor":{"lock_window":False}}
                for i in range(2)]}
        clone_fields = {"clone_source_id":ids[0],"clone_target_pid":str(g.pids[2]),"clone_name":"Owned utility replica",
            "clone_motion":"false","clone_wheel":"false","clone_horizontal_wheel":"false",
            "clone_button_primary":"false","clone_button_secondary":"false","clone_button_middle":"false",
            "clone_button_back":"true","clone_button_forward":"false"}
        step = ui.step
        steps=[step("start","Start routing"),step("settings","Settings"),
            step("automation-category","Settings category Automation"),step("replicas","Virtual Replicas"),
            step("start-clone","Start virtual replica",clone_fields),step("select-clone","Select replica 3"),
            step("stop-clone","Stop selected virtual replica"),step("replica-details","Stable identity details"),
            step("stopped-action","Stop selected virtual replica",disabled=True)]
        assert len(steps)==9
        profile["replication"]=copy.deepcopy(graph) # Explicit fixture, not a reconstructed prior live ACK.
        g.evidence["retained_prefix"]={"path":str(args.from_run/"evidence.json"),
            "sha256":hashlib.sha256((args.from_run/"evidence.json").read_bytes()).hexdigest(),
            "result":prefix["result"],"observations":18,"reason":"VU-only corrected pixels; Macro and Graph/status evidence retained separately"}
        provenance = g.run/"runner-source";provenance.mkdir()
        for name in (Path(__file__).name,"owned-gui-geometry.py"):
            shutil.copy2(Path(__file__).with_name(name),provenance/name)
        g.evidence["scope"] = "VU-only corrected responsive rows and identities; 8 clicks + 1 disabled observation, three frames. No Agent/Macro/Graph/status replay or permission setup."
        g.execute(profile,prefs,steps,verify,lambda gate:gate.main_hwnd() is not None,after)
        assert len(g.frames)==3
        g.evidence.update(result="PENDING INDEPENDENT VISUAL REVIEW",control_state_result="PASS",captures=g.frames)
        g.save();return g.run
    finally:
        g.close();lifecycle.remove_startup_key(g.startup_key)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready",action="store_true")
    parser.add_argument("--from-run",type=Path)
    for name in ("executable","x86-executable","runtime"):parser.add_argument("--"+name,type=Path,required=True)
    args = parser.parse_args()
    assert args.ready and args.from_run,"Await frozen correction READY; exact prior raw run required"
    workflow(args)


if __name__=="__main__":main()
