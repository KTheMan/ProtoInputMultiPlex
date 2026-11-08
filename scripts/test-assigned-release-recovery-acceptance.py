"""Finite assigned-route failed-release recovery gate; no launch before ROOT READY."""
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
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(filename))
    result=importlib.util.module_from_spec(spec);spec.loader.exec_module(result);return result


def digest(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def no_repeat(function):
    def guarded(*args,**kwargs):
        try:return function(*args,**kwargs)
        except(OSError,ValueError)as error:
            raise RuntimeError("Imperative phase cannot be retried after a transient read/write: "+function.__name__)from error
    return guarded


def freeze(args):
    assert args.ready,"Await final source/producer/harness review and ROOT READY"
    assert digest(args.producer)==args.producer_sha256.lower()
    p=json.loads(args.producer.read_text(encoding="utf-8"));repo=Path(__file__).resolve().parent.parent
    assert p["result"]=="PASS" and p["source_before_sha256"]==p["source_after_sha256"]
    assert len(p["checks"])==5 and all(c["exit_code"]==0 for c in p["checks"])
    assert all(digest(repo/n)==h for n,h in p["source_after_sha256"].items())
    assert digest(args.executable)==p["build_sha256"]["x64"]
    assert digest(args.x86_executable)==p["build_sha256"]["i686"]
    assert all(digest(args.runtime/n)==h for n,h in p["runtime_sha256"].items())
    return p


def workflow(args):
    lifecycle=module("assigned_release_lifecycle","test-app-lifecycle-ui.py")
    transport=module("assigned_release_mcp","test-connected-agents-acceptance.py")
    comparison=module("assigned_release_json","test-keyboard-persistence-ui.py")
    Base,ui,hotkeys,geometry=lifecycle.gate_type()
    ids=["assigned-release-a","assigned-release-b"]
    class Gate(Base):
        def __init__(self):
            super().__init__(args,"assigned-release-recovery",offline=False,count=2)
            self.targets=[];self.windows=[];self.layers=set();self.client=None
            self.calls=[];self.mutations=0;self.phase=0;self.frames={};self.descriptor=None
            self.expected_profile=None;self.fresh_profile=None;self.fresh_path=self.run/"fresh assigned profile.json"
        def snapshot(self):return ui.eventually(lambda:super(Gate,self).snapshot(),lambda v:True,3)
        def probes(self):
            return ui.eventually(lambda:[ui.read_json(self.run/f"probe-{i}.json")for i in range(len(self.targets))],lambda v:True,3)
        def fixture_pair(self):
            offset=len(self.targets)
            for i in range(offset,offset+2):
                proc=subprocess.Popen([str(self.images[0]),"probe","--automation","--report",str(self.run/f"probe-{i}.json")],creationflags=subprocess.CREATE_NO_WINDOW)
                self.targets.append(proc);self.owned.append(proc)
                hwnd=ui.eventually(lambda:geometry.owned_window(proc.pid),bool,10)
                assert geometry.user32.SetWindowPos(hwnd,wintypes.HWND(-1),40+460*(i%2),50,420,360,0x10)
                self.windows.append(int(hwnd))
            ui.eventually(self.probes,lambda rs:len(rs)==len(self.targets)and all(r["frames"]>2 for r in rs),8)
            return [p.pid for p in self.targets[offset:]]
        def call(self,name,arguments=None,error=False):
            if name!="routing_state":self.mutations+=1
            assert self.mutations<=20
            entry={"name":name,"arguments":arguments or {},"expected_error":error,"started_ns":time.monotonic_ns()}
            self.calls.append(entry);self.evidence["mcp_calls"]=self.calls;self.save()
            try:entry["result"]=self.client.tool(name,arguments,error=error)
            except(OSError,ValueError)as failure:raise RuntimeError("MCP request failed; never resubmit automatically")from failure
            entry["finished_ns"]=time.monotonic_ns();self.save()
            return entry["result"]
        def state(self):
            result=self.call("routing_state")
            if hasattr(self,"client_id"):assert result["agent_session"]["client"]==self.client_id
            return result
        def input(self,action,**fields):return self.call("routing_input",{"action":action,**fields})
        def failed_input(self,action,**fields):
            result=self.call("routing_input",{"action":action,**fields},error=True)
            assert result["isError"],result
            return result
        def keys(self,user,key,pressed):return self.input("key",user=user,key=key,pressed=pressed)
        def pending(self,rows,count=2):
            assert len(rows)==2 and rows[0]["pending_releases"]==count and rows[1]["pending_releases"]==0,rows
            assert all(r["pid"]==p.pid for r,p in zip(rows,self.targets[:2])),rows
            assert rows[0]["keys"]==[117] and rows[0]["buttons"]==[4],rows
        def root_alive(self,index):
            assert self.targets[index].poll()is None and ui.user32.IsWindow(self.windows[index])
        @no_repeat
        def connect_failure(self):
            if self.phase:return
            paths=list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{self.gui.pid}-*.json"));assert len(paths)==1
            self.descriptor=paths[0]
            self.client=transport.ConnectedMcp(self.images[0],self.descriptor,self.run/"connected-mcp.stderr.log")
            self.owned.append(self.client.process)
            self.evidence["mcp_pid"]=self.client.process.pid
            initial=self.state();assert initial["running"] and len(initial["users"])==2
            self.client_id=initial["agent_session"]["client"];assert self.client_id and self.client_id>0
            self.evidence["connected_client_id"]=self.client_id
            assert set(initial["agent_session"]["policy"]["user_ids"])==set(ids)
            assert set(initial["agent_session"]["policy"]["allowed_pids"])==set(self.pids)
            self.expected_profile=copy.deepcopy(self.snapshot()["profile"])
            for i,r in enumerate(self.probes()[:2]):
                child=r["capture_hwnd"];owner=wintypes.DWORD()
                assert geometry.user32.GetWindowThreadProcessId(child,ctypes.byref(owner)) and owner.value==self.pids[i]
                geometry.user32.GetParent.argtypes=[wintypes.HWND];geometry.user32.GetParent.restype=wintypes.HWND
                assert geometry.user32.GetParent(child)==self.windows[i]
                name=ctypes.create_unicode_buffer(32);geometry.user32.GetClassNameW.argtypes=[wintypes.HWND,wintypes.LPWSTR,ctypes.c_int]
                assert geometry.user32.GetClassNameW(child,name,32)and name.value.casefold()=="edit"
                rect=geometry.Rect();assert geometry.user32.GetWindowRect(child,ctypes.byref(rect))
                self.input("move_to",user=i,x=rect.left+80,y=rect.top+60)
                self.input("button",user=i,button="primary",pressed=True)
                self.input("button",user=i,button="primary",pressed=False)
            self.keys(0,"f6",True);self.input("button",user=0,button="secondary",pressed=True);self.keys(1,"f7",True)
            reports=ui.eventually(self.probes,lambda rs:rs[0]["held_keys"]==[117]and rs[0]["buttons"]&2 and rs[1]["held_keys"]==[118],8)
            state=self.state();self.layers.update(r["overlay"]["hwnd"]for r in state["users"])
            assert len(self.layers)==2 and all(h and ui.user32.IsWindow(h)for h in self.layers)
            self.evidence["initial_native"]=reports
            child=reports[0]["capture_hwnd"]
            assert geometry.user32.PostMessageW(child,0x10,0,0)
            ui.eventually(lambda:not ui.user32.IsWindow(child),bool,5);self.root_alive(0)
            self.failed_input("key",user=0,key="f6",pressed=False)
            self.failed_input("button",user=0,button="secondary",pressed=False)
            self.failed_input("key",user=0,key="f9",pressed=True)
            before=self.state();self.pending(before["users"])
            settings=copy.deepcopy(next(r["settings"]for r in before["user_settings"]if r["user"]==0));candidate=copy.deepcopy(settings);candidate["speed_x"]=1.25
            failed=self.call("routing_settings",{"user":0,"settings":candidate},error=True);assert failed["isError"],failed
            after=self.state();self.pending(after["users"])
            comparison.strict_equal(next(r["settings"]for r in after["user_settings"]if r["user"]==0),settings)
            comparison.strict_equal(self.snapshot()["profile"],self.expected_profile)
            reports=self.probes();assert reports[0]["held_keys"]==[117]and reports[0]["buttons"]&2 and reports[1]["held_keys"]==[118]
            assert [(e["vk"],e["pressed"])for e in reports[0]["key_events"]]==[(117,True)]
            self.evidence["failed_releases"]={"original_hwnd":child,"original_pid":self.pids[0],"parent_alive":True,"before":before,"after":after,"native":reports}
            self.phase=1
        def frame(self,name):
            if name in self.frames:return True
            self.root_alive(0)
            rows=ui.read_json(self.run/"ui.state.json");self.pending(rows)
            # Hold requested real state across async screenshot cadence. Fresh
            # writes are only provenance checks; actual content needs review.
            frame=self.frames.setdefault("hold-"+name,{"started":time.time(),"writes":[]})
            grace=frame["started"]+3.2
            if time.time()<grace:return False
            path=self.run/"ui.png"
            try:
                from PIL import Image
                stat=path.stat()
                if stat.st_mtime<=grace:return False
                raw=path.read_bytes();Image.open(__import__("io").BytesIO(raw)).load()
                identity=[stat.st_mtime_ns,len(raw),hashlib.sha256(raw).hexdigest()]
                if path.stat().st_mtime_ns!=stat.st_mtime_ns:return False
                if not frame["writes"]or frame["writes"][-1][0]!=identity[0]:frame["writes"].append(identity)
                if len(frame["writes"])<2:return False
                shot=geometry.copy_owned_product_frame(path,self.run/(name+".png"),self.main_hwnd(),grace)
                if not shot:return False
                assert shot["geometry"]["client_logical"]==[360.0,360.0]
                self.frames[name]=shot;self.evidence["originals"]=self.frames;self.save();return True
            except(OSError,ValueError):return False
        @no_repeat
        def after_global_release(self):
            if self.phase>=2:return
            state=self.state();assert state["running"];self.pending(state["users"])
            rs=ui.eventually(self.probes,lambda rs:not rs[1]["held_keys"],5)
            assert [(e["vk"],e["pressed"])for e in rs[1]["key_events"]]==[(118,True),(118,False)]
            resumed=self.input("resume")
            assert resumed["running"]and not resumed["paused"]
            assert resumed["agent_session"]["client"]==self.client_id
            self.pending(resumed["users"])
            self.evidence["resume_during_pending"]=resumed
            self.keys(1,"f5",True);self.keys(1,"f5",False)
            rs=ui.eventually(self.probes,lambda rs:len(rs[1]["key_events"])==4,5)
            assert [(e["vk"],e["pressed"])for e in rs[1]["key_events"]]==[(118,True),(118,False),(116,True),(116,False)]
            self.evidence["healthy_B_after_global_release"]={"state":self.state(),"native":rs};self.phase=2
        def recovery(self):return ui.read_json(self.run/"ui.recovery.json")
        def quarantined(self):
            r=self.recovery();assert r["phase"]=="quarantined" and not r["can_start"] and r["error"],r
            assert {t["pid"]for t in r["targets"]}==set(self.pids) and all(t["affected"]for t in r["targets"]),r
            return r
        def close_target(self,index):
            proc=self.targets[index]
            if proc.poll()is None:
                assert geometry.user32.PostMessageW(self.windows[index],0x10,0,0)
                proc.wait(timeout=8)
            assert proc.returncode==0
        @no_repeat
        def recover_and_fresh(self):
            if self.phase>=4:return
            self.evidence["quarantine_both_alive"]=self.quarantined();self.root_alive(0);self.root_alive(1)
            self.close_target(0)
            r=ui.eventually(self.recovery,lambda v:any(t["pid"]==self.pids[0]and t["status"]=="exited"for t in v["targets"]),5)
            assert not r["can_start"]and self.targets[1].poll()is None
            self.evidence["quarantine_only_B_alive"]=r
            self.close_target(1)
            cleared=ui.eventually(self.recovery,lambda v:v["can_start"]and v["phase"]=="stopped",8)
            self.evidence["recovery_after_both_exit"]=cleared
            self.fresh_pids=self.fixture_pair();self.fresh_profile=copy.deepcopy(self.expected_profile)
            for seat,pid in zip(self.fresh_profile["seats"],self.fresh_pids):seat["target_pid"]=pid
            assert not self.fresh_path.exists();ui.put(self.fresh_path,self.fresh_profile)
            self.evidence["fresh_owned_pids"]=self.fresh_pids;self.phase=4

    g=Gate()
    try:
        g.pids=g.fixture_pair()
        profile={"version":1,"desktop":None,"concurrent":True,"seats":[{"id":ids[i],"name":"Assigned "+("Alpha"if i==0 else"Beta"),"mouse":f"virtual://mouse/{i+1}","keyboard":f"virtual://keyboard/{i+1}","target_pid":g.pids[i],"cursor":{"lock_window":False}}for i in range(2)]}
        prefs={"version":1,"appearance_mode":"dark","zoom_percent":100,"launch_minimized":False,"minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,"shortcuts":[]}
        steps=[ui.step("start","Start routing"),ui.step("settings","Settings"),ui.step("agent-category","Settings category Agent access"),ui.step("agent","Agent Access"),
               ui.step("enable","Enable agent access",{"agent_users":','.join(ids),"agent_pids":','.join(map(str,g.pids)),"agent_session_control":"true","agent_capture":"false","agent_app_preferences":"false"}),
               ui.step("home","Back to Home"),ui.step("details","Input release details"),ui.step("release","Release all input"),ui.step("close-details","Close release details"),ui.step("failed-stop","Stop routing"),
               ui.step("blocked-start","Start routing",disabled=True),ui.step("fresh-settings","Settings"),ui.step("fresh-users","Settings category Users & devices"),ui.step("profiles","Profiles & backend"),
               ui.step("load-fresh","Load profile",{"profile_path":g.fresh_path}),ui.step("fresh-home","Back to Home"),ui.step("fresh-start","Start routing"),ui.step("fresh-stop","Stop routing")]
        assert len(steps)==18
        def before(gate):
            hwnd=gate.main_hwnd()
            if not hwnd:return False
            if geometry.geometry(hwnd)["client_logical"]!=[360.0,360.0]:geometry.resize_owned_client(hwnd,[360,360]);return False
            return True
        def verify(name,sample):
            comparison.strict_equal(ui.read_json(g.preferences_path),prefs)
            if name=="start":
                if len(sample["state"])!=2:return False
                assert all(r["pending_releases"]==0 for r in sample["state"])
            if name=="enable":g.connect_failure()
            if g.expected_profile is not None:
                expected=g.fresh_profile if name in("load-fresh","fresh-home","fresh-start","fresh-stop")else g.expected_profile
                if name=="load-fresh"and sample["profile_state"]["profile"]!=expected:
                    comparison.strict_equal(sample["profile_state"]["profile"],g.expected_profile);return False
                comparison.strict_equal(sample["profile_state"]["profile"],expected)
            if name=="home":return g.frame("assigned-release-home")
            if name=="details":return g.frame("assigned-release-details")
            if name=="release":g.after_global_release()
            if name=="close-details"and g.phase==2:g.keys(1,"f7",True);g.phase=3
            if name=="failed-stop":
                r=g.recovery()
                if r["phase"]in("running","stopping"):return False
                g.evidence["failed_stop_recovery"]=g.quarantined()
                rs=ui.eventually(g.probes,lambda rs:len(rs[1]["key_events"])==6 and not rs[1]["held_keys"],5)
                assert [(e["vk"],e["pressed"])for e in rs[1]["key_events"]]==[(118,True),(118,False),(116,True),(116,False),(118,True),(118,False)]
                g.evidence["B_after_failed_stop"]=rs[1]
                assert not list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{g.gui.pid}-*.json"))
                g.evidence["agent_reset_limit"]="Descriptor disappearance observed. Policy/draft clear is source-reviewed; ui.agent.json is not refreshed on Home terminal reset."
                if g.client:g.client.close();g.client=None
            if name=="blocked-start":g.recover_and_fresh()
            if name=="load-fresh":
                if sample["profile_state"]["status"]!="Profile loaded. Verify target PIDs before attaching.":return False
                assert sample["profile_state"]["active_path"]==str(g.fresh_path)
            if name=="fresh-start":
                if len(sample["state"])!=2:return False
                assert [r["pid"]for r in sample["state"]]==g.fresh_pids
                assert all(r["pending_releases"]==0 for r in sample["state"])
                new_layers={r["overlay"]["hwnd"]for r in sample["state"]};assert len(new_layers)==2 and all(ui.user32.IsWindow(h)for h in new_layers)
                g.layers.update(new_layers)
            if name=="fresh-stop":
                if g.recovery()["phase"]!="stopped":return False
                if any(ui.user32.IsWindow(h)for h in g.layers):return False
                assert not g.recovery()["error"] and g.recovery()["can_start"]
                assert not list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{g.gui.pid}-*.json"))
                assert g.mutations==17;g.evidence["mcp_mutations"]=g.mutations
            return True
        def after(gate):
            for i in range(len(g.targets)):g.close_target(i)
            gate.expected_exit=True;assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0)
            gate.gui.wait(timeout=10);assert gate.gui.returncode==0 and not hotkeys.lifecycle_window(gate.gui.pid)
            return True
        provenance=g.run/"runner-source";provenance.mkdir()
        shutil.copy2(Path(__file__),provenance/Path(__file__).name)
        g.evidence.update(producer=str(args.producer),producer_sha256=digest(args.producer),owned_pids=g.pids,scope="Assigned concurrent release recovery; no old disconnected-client replay.")
        g.execute(profile,prefs,steps,verify,before_controls=before,after_controls=after)
        g.evidence["result"]="PENDING INDEPENDENT DATA/PIXEL REVIEW"
    finally:
        if g.client:
            try:g.client.close()
            except Exception as error:
                g.evidence.setdefault("cleanup_errors",[]).append(repr(error))
                if g.client.process.poll()is None:g.client.process.kill();g.client.process.wait(timeout=10)
                g.client.stderr.close()
        # Base owns only disposable processes. Always attempt normal own-root close
        # before its last-resort bounded termination path.
        for i,process in enumerate(g.targets):
            if process.poll()is None:
                try:g.close_target(i)
                except Exception as error:g.evidence.setdefault("cleanup_errors",[]).append(repr(error))
        g.close();lifecycle.remove_startup_key(g.startup_key)
        g.evidence["known_overlays_closed"]=all(not ui.user32.IsWindow(h)for h in g.layers)
        g.evidence["owned_exit_codes"]={str(p.pid):p.returncode for p in g.owned}
        if g.evidence.get("cleanup_errors")or not g.evidence["known_overlays_closed"]:g.evidence["result"]="NOT ACCEPTED"
        g.save()


def main():
    parser=argparse.ArgumentParser();parser.add_argument("--ready",action="store_true")
    for name in("executable","x86-executable","runtime","producer"):parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--producer-sha256",required=True)
    args=parser.parse_args();freeze(args);workflow(args)


if __name__=="__main__":main()
