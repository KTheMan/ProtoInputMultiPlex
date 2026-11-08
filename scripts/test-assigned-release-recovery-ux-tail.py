"""Missing-only compact Home/dialog correction gate; no launch before ROOT READY."""
import argparse
import copy
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


def module(name,filename):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(filename));value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value


def digest(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def freeze(args):
    assert args.ready,"Await final source review/producer/ROOT READY"
    R=Path(__file__).resolve().parent.parent
    prior=R/"runtime/acceptance-graph-assigned-release-recovery-20261015-080418"
    assert digest(prior/"evidence.json")=="6f4aa7fa49c8fd88af5e186d54060cf004ad61d1b238c6f9425eb960f8af6836"
    assert digest(prior/"independent-review.json")=="6dc4e6532a58232db58a0c94de37194a3ff6fa3f638e80f9c40b890c9f4b5267"
    assert digest(prior/"runner-source/test-assigned-release-recovery-acceptance.py")=="9811abb582d79c5fc21ac7f5763cadf0d7e58be23485ed333d19649b72e9b1ca"
    B=R/"runtime/producer-assigned-release-recovery-20261015-080115/verification.json"
    assert digest(B)=="8d57dd3e4f94646caaf49927c08982478b959ce6d087bded28f432db4cdb614b"
    old=json.loads(B.read_text(encoding="utf-8"));p=json.loads(args.producer.read_text(encoding="utf-8"))
    assert digest(args.producer)==args.producer_sha256.lower()and p["result"]=="PASS"
    assert p["source_before_sha256"]==p["source_after_sha256"]and len(p["checks"])==5 and all(c["exit_code"]==0 for c in p["checks"])
    assert set(old["source_after_sha256"])==set(p["source_after_sha256"])
    changed={n.replace("\\","/")for n,v in p["source_after_sha256"].items()if old["source_after_sha256"][n]!=v}
    assert changed=={"crates/protoinputmultiplex/src/gui.rs"},changed
    assert all(digest(R/n)==v for n,v in p["source_after_sha256"].items())
    assert p["runtime_sha256"]==old["runtime_sha256"]and all(digest(args.runtime/n)==v for n,v in p["runtime_sha256"].items())
    assert digest(args.executable)==p["build_sha256"]["x64"]and digest(args.x86_executable)==p["build_sha256"]["i686"]
    return prior


def workflow(args,prior):
    lifecycle=module("release_ux_base","test-app-lifecycle-ui.py")
    connected=module("release_ux_mcp","test-connected-agents-acceptance.py")
    comparison=module("release_ux_strict","test-keyboard-persistence-ui.py")
    Base,ui,hotkeys,geometry=lifecycle.gate_type()
    ids=["release-ux-a","release-ux-b"]
    class Gate(Base):
        def __init__(self):
            super().__init__(args,"assigned-release-recovery-ux-tail",offline=False,count=2)
            self.targets=[];self.windows=[];self.layers=set();self.client=None;self.stage=0;self.calls=[];self.mutations=0;self.frames={};self.capture_holds={};self.expected_profile=None
        def snapshot(self):return ui.eventually(lambda:super(Gate,self).snapshot(),lambda v:True,3)
        def probes(self):return ui.eventually(lambda:[ui.read_json(self.run/f"probe-{i}.json")for i in range(2)],lambda v:True,3)
        def fixture(self):
            for i in range(2):
                proc=subprocess.Popen([str(self.images[0]),"probe","--automation","--report",str(self.run/f"probe-{i}.json")],creationflags=subprocess.CREATE_NO_WINDOW)
                self.targets.append(proc);self.owned.append(proc)
                hwnd=ui.eventually(lambda:geometry.owned_window(proc.pid),bool,10)
                assert geometry.user32.SetWindowPos(hwnd,wintypes.HWND(-1),40+460*i,50,420,360,0x10)
                self.windows.append(int(hwnd))
            ui.eventually(self.probes,lambda r:all(s["frames"]>2 for s in r),8)
            return [p.pid for p in self.targets]
        def call(self,name,arguments=None,error=False):
            if name!="routing_state":self.mutations+=1
            assert self.mutations<=5
            rec={"name":name,"arguments":arguments or {},"expected_error":error,"started_ns":time.monotonic_ns()};self.calls.append(rec);self.evidence["mcp_calls"]=self.calls;self.save()
            try:rec["result"]=self.client.tool(name,arguments,error=error)
            except(OSError,ValueError)as e:raise RuntimeError("Do not repeat an imperative MCP request")from e
            rec["finished_ns"]=time.monotonic_ns();self.save();return rec["result"]
        def pending(self):
            assert self.targets[0].poll()is None and ui.user32.IsWindow(self.windows[0])
            rows=ui.read_json(self.run/"ui.state.json")
            assert len(rows)==2 and rows[0]["pending_releases"]==1 and rows[0]["keys"]==[117]and rows[1]["pending_releases"]==0,rows
            return rows
        def create_pending(self):
            if self.stage:return
            try:
                paths=list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{self.gui.pid}-*.json"));assert len(paths)==1
                self.client=connected.ConnectedMcp(self.images[0],paths[0],self.run/"mcp.stderr.log");self.owned.append(self.client.process);self.evidence["mcp_pid"]=self.client.process.pid
                initial=self.call("routing_state");assert initial["running"]and set(initial["agent_session"]["policy"]["allowed_pids"])==set(self.pids)
                child=self.probes()[0]["capture_hwnd"];owner=wintypes.DWORD()
                assert geometry.user32.GetWindowThreadProcessId(child,ctypes.byref(owner))and owner.value==self.pids[0]
                geometry.user32.GetParent.argtypes=[wintypes.HWND];geometry.user32.GetParent.restype=wintypes.HWND
                assert geometry.user32.GetParent(child)==self.windows[0]
                title=ctypes.create_unicode_buffer(32);geometry.user32.GetClassNameW.argtypes=[wintypes.HWND,wintypes.LPWSTR,ctypes.c_int]
                assert geometry.user32.GetClassNameW(child,title,32)and title.value.casefold()=="edit"
                rect=geometry.Rect();assert geometry.user32.GetWindowRect(child,ctypes.byref(rect))
                self.call("routing_input",{"action":"move_to","user":0,"x":rect.left+80,"y":rect.top+60})
                for down in(True,False):self.call("routing_input",{"action":"button","user":0,"button":"primary","pressed":down})
                self.call("routing_input",{"action":"key","user":0,"key":"f6","pressed":True})
                native=ui.eventually(self.probes,lambda r:r[0]["held_keys"]==[117]and not r[0]["buttons"],8)
                assert geometry.user32.PostMessageW(child,0x10,0,0)
                ui.eventually(lambda:not ui.user32.IsWindow(child),bool,5)
                assert self.targets[0].poll()is None and ui.user32.IsWindow(self.windows[0])
                rejected=self.call("routing_input",{"action":"key","user":0,"key":"f6","pressed":False},error=True);assert rejected["isError"]
                state=self.call("routing_state");assert state["running"]and state["agent_session"]["client"]==initial["agent_session"]["client"]
                assert state["users"][0]["pending_releases"]==1 and state["users"][1]["pending_releases"]==0
                self.layers.update(r["overlay"]["hwnd"]for r in state["users"]);assert len(self.layers)==2 and all(h and ui.user32.IsWindow(h)for h in self.layers)
                self.evidence["pending_setup"]={"state":state,"native_before_child_close":native,"original_child":child,"original_pid":self.pids[0],"parent_alive":True}
                self.expected_profile=copy.deepcopy(self.snapshot()["profile"]);self.stage=1
            except(OSError,ValueError)as e:raise RuntimeError("Pending setup cannot silently retry")from e
        def frame(self,name):
            if name in self.frames:return True
            self.pending()
            hold=self.capture_holds.setdefault(name,{"stage":"normal","writes":[]})
            hwnd=self.main_hwnd();actual=geometry.geometry(hwnd)
            if hold["stage"]=="normal":
                if actual["client_logical"]!=[360.0,360.0]:geometry.resize_owned_client(hwnd,[360,360]);return False
                hold["normal_geometry"]=actual;geometry.resize_owned_client(hwnd,[320,360]);hold["stage"]="minimum";return False
            if actual["client_logical"]!=[320.0,360.0]:return False
            hold.setdefault("minimum_geometry",actual);hold.setdefault("started",time.time())
            grace=hold["started"]+3.2
            if time.time()<grace:return False
            source=self.run/"ui.png"
            try:
                from PIL import Image
                stat=source.stat()
                if stat.st_mtime<=grace:return False
                raw=source.read_bytes();img=Image.open(io.BytesIO(raw));img.load()
                if img.size!=(320,360)or source.stat().st_mtime_ns!=stat.st_mtime_ns:return False
                item=[stat.st_mtime_ns,len(raw),hashlib.sha256(raw).hexdigest()]
                if not hold["writes"]or item[0]!=hold["writes"][-1][0]:hold["writes"].append(item)
                if len(hold["writes"])<2:return False
                shot=geometry.copy_owned_product_frame(source,self.run/(name+".png"),hwnd,grace)
                if not shot:return False
                assert shot["geometry"]["client_logical"]==[320.0,360.0]
                self.frames[name]=shot;self.evidence["originals"]=self.frames;self.evidence["capture_holds"]=self.capture_holds;self.save();return True
            except(OSError,ValueError):return False
        def close_target(self,i):
            if self.targets[i].poll()is None:
                assert geometry.user32.PostMessageW(self.windows[i],0x10,0,0);self.targets[i].wait(timeout=8)
            assert self.targets[i].returncode==0
    g=Gate()
    prefs={"version":1,"appearance_mode":"dark","zoom_percent":100,"launch_minimized":False,"minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,"shortcuts":[]}
    try:
        g.pids=g.fixture()
        profile={"version":1,"desktop":None,"concurrent":True,"seats":[{"id":ids[i],"name":"Assigned "+("Alpha"if i==0 else"Beta"),"mouse":f"virtual://mouse/{i+1}","keyboard":f"virtual://keyboard/{i+1}","target_pid":g.pids[i],"cursor":{"lock_window":False}}for i in range(2)]}
        steps=[ui.step("start","Start routing"),ui.step("settings","Settings"),ui.step("agent-category","Settings category Agent access"),ui.step("agent","Agent Access"),
               ui.step("enable","Enable agent access",{"agent_users":','.join(ids),"agent_pids":','.join(map(str,g.pids)),"agent_session_control":"false","agent_capture":"false","agent_app_preferences":"false"}),
               ui.step("home","Back to Home"),ui.step("details","Input release details"),ui.step("close-details","Close release details"),ui.step("stop","Stop routing")]
        assert len(steps)==9
        def verify(name,sample):
            comparison.strict_equal(ui.read_json(g.preferences_path),prefs)
            if name=="enable":g.create_pending()
            if g.expected_profile is not None:comparison.strict_equal(sample["profile_state"]["profile"],g.expected_profile)
            if name=="home":return g.frame("release-home-minimum")
            if name=="details":return g.frame("release-dialog-minimum")
            if name=="stop":
                r=ui.read_json(g.run/"ui.recovery.json")
                if r["phase"]in("running","stopping"):return False
                assert r["phase"]=="quarantined"and not r["can_start"]and {t["pid"]for t in r["targets"]}==set(g.pids),r
                assert all(t["affected"]for t in r["targets"])
                g.evidence["stop_quarantine"]=r;assert g.mutations==5;g.evidence["mcp_mutations"]=g.mutations
            return True
        def after(gate):
            if gate.client:gate.client.close();gate.client=None
            for i in range(2):gate.close_target(i)
            cleared=ui.eventually(lambda:ui.read_json(gate.run/"ui.recovery.json"),lambda r:r["can_start"]and r["phase"]=="stopped",8)
            gate.evidence["cleanup_recovery"]=cleared
            assert not list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{gate.gui.pid}-*.json"))
            gate.expected_exit=True;assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0);gate.gui.wait(timeout=10)
            assert gate.gui.returncode==0 and not hotkeys.lifecycle_window(gate.gui.pid)
            return True
        provenance=g.run/"runner-source";provenance.mkdir();shutil.copy2(Path(__file__),provenance/Path(__file__).name)
        g.evidence.update(producer=str(args.producer),producer_sha256=digest(args.producer),prior_functional_run=str(prior),owned_pids=g.pids,scope="GUI-only correction;5MCPpending setup,9GUIclicks,two320originals/360geometryonly.")
        g.execute(profile,prefs,steps,verify,after_controls=after)
        g.evidence["result"]="PENDING INDEPENDENT GUI DATA/PIXEL REVIEW"
    finally:
        if g.client:
            try:g.client.close()
            except Exception as error:
                g.evidence.setdefault("cleanup_errors",[]).append(repr(error))
                if g.client.process.poll()is None:g.client.process.kill();g.client.process.wait(timeout=10)
                g.client.stderr.close()
        for i,p in enumerate(g.targets):
            if p.poll()is None:
                try:g.close_target(i)
                except Exception as error:g.evidence.setdefault("cleanup_errors",[]).append(repr(error))
        g.close();lifecycle.remove_startup_key(g.startup_key)
        g.evidence["known_overlays_closed"]=all(not ui.user32.IsWindow(h)for h in g.layers);g.evidence["owned_exit_codes"]={str(p.pid):p.returncode for p in g.owned}
        if g.evidence.get("cleanup_errors")or not g.evidence["known_overlays_closed"]:g.evidence["result"]="NOT ACCEPTED"
        g.save()


def main():
    p=argparse.ArgumentParser();p.add_argument("--ready",action="store_true")
    for name in("executable","x86-executable","runtime","producer"):p.add_argument("--"+name,type=Path,required=True)
    p.add_argument("--producer-sha256",required=True);args=p.parse_args();prior=freeze(args);workflow(args,prior)


if __name__=="__main__":main()
