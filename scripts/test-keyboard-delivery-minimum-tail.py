"""Missing-only minimum Keyboard presentation; ROOT READY required, no input packets."""
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
    assert args.ready,"Await frozen producer/source approval/ROOT READY"
    repo=Path(__file__).resolve().parent.parent;old_run=repo/"runtime/acceptance-graph-keyboard-delivery-20261015-090348"
    assert digest(old_run/"evidence.json")=="ac69456993f280b3c2175c5f4da40d52f5299d571e3d428173896be73b2485d4"
    assert digest(old_run/"independent-review.json")=="c40b35ac27dfc22658e8e38c56c5c615cabe00bdc713daeb651dd103c6599ba9"
    for name,h in {"keyboard-focused-normal.png":"c2836ee07fda9834c38045905b83bde14d0609e7e22f70b7927c0e0800a7542f","keyboard-external-ack-preserved-draft.png":"33ebe31658c3f16b8df6e8a624c84f06d09533ac9f4572f1001e8ad7718bc568","keyboard-disabled-minimum.png":"2cf2ce5e2f0df51a104391706cc145caa380d416ae18cd1bc95f8e7ec471c606"}.items():assert digest(old_run/name)==h
    old=json.loads((repo/"runtime/producer-keyboard-delivery-20261015-090154/verification.json").read_text(encoding="utf-8"))
    assert digest(args.producer)==args.producer_sha256.lower();p=json.loads(args.producer.read_text(encoding="utf-8"))
    assert p["result"]=="PASS"and p["source_before_sha256"]==p["source_after_sha256"]and len(p["checks"])==5 and all(c["exit_code"]==0 for c in p["checks"])
    assert set(old["source_after_sha256"])==set(p["source_after_sha256"])
    changed={n.replace('\\','/')for n,h in old["source_after_sha256"].items()if p["source_after_sha256"][n]!=h}
    assert changed=={"crates/protoinputmultiplex/src/gui.rs"},changed
    assert all(digest(repo/n)==h for n,h in p["source_after_sha256"].items())
    assert old["runtime_sha256"]==p["runtime_sha256"]and all(digest(args.runtime/n)==h for n,h in p["runtime_sha256"].items())
    assert digest(args.executable)==p["build_sha256"]["x64"]and digest(args.x86_executable)==p["build_sha256"]["i686"]
    return p,old_run


def workflow(args,producer,prior):
    lifecycle=module("minimum_keyboard_base","test-app-lifecycle-ui.py");strict=module("minimum_keyboard_exact","test-keyboard-persistence-ui.py")
    Base,ui,hotkeys,geometry=lifecycle.gate_type()
    class Gate(Base):
        def __init__(self):
            super().__init__(args,"keyboard-delivery-minimum-tail",offline=False,count=1)
            self.target=None;self.root=None;self.layers=set();self.expected=None;self.original=None;self.hold=None
        def probes(self):return ui.eventually(lambda:[ui.read_json(self.run/"probe-0.json")],lambda v:True,3)
        def snapshot(self):return ui.eventually(lambda:super(Gate,self).snapshot(),lambda v:True,3)
        def fixture(self):
            self.target=subprocess.Popen([str(self.images[0]),"probe","--automation","--report",str(self.run/"probe-0.json")],creationflags=subprocess.CREATE_NO_WINDOW);self.owned.append(self.target)
            self.root=ui.eventually(lambda:geometry.owned_window(self.target.pid),bool,10)
            ui.eventually(self.probes,lambda rs:rs[0]["frames"]>2,8)
        def minimum(self,phase):
            hwnd=self.main_hwnd();actual=geometry.geometry(hwnd)
            if actual["client_logical"]!=[320.0,360.0]:geometry.resize_owned_client(hwnd,[320,360]);return False
            self.evidence.setdefault("minimum_geometry",{})[phase]=actual;return True
        def frame(self):
            if self.original:return True
            if not self.minimum("checkbox-draft"):return False
            if self.hold is None:self.hold={"started":time.time(),"writes":[]}
            grace=self.hold["started"]+3.2
            if time.time()<grace:return False
            try:
                from PIL import Image
                path=self.run/"ui.png";stat=path.stat()
                if stat.st_mtime<=grace:return False
                raw=path.read_bytes();picture=Image.open(io.BytesIO(raw));picture.load()
                if picture.size!=(320,360)or path.stat().st_mtime_ns!=stat.st_mtime_ns:return False
                item=[stat.st_mtime_ns,len(raw),hashlib.sha256(raw).hexdigest()]
                if not self.hold["writes"]or item[0]!=self.hold["writes"][-1][0]:self.hold["writes"].append(item)
                if len(self.hold["writes"])<2:return False
                self.original=geometry.copy_owned_product_frame(path,self.run/"keyboard-off-draft-minimum.png",self.main_hwnd(),grace)
                if not self.original:return False
                self.evidence["original"]=self.original;self.evidence["capture_hold"]=self.hold;self.save();return True
            except(OSError,ValueError):return False
        def close_target(self):
            if self.target and self.target.poll()is None:
                assert geometry.user32.PostMessageW(self.root,0x10,0,0);self.target.wait(timeout=8)
            if self.target:assert self.target.returncode==0
    g=Gate();prefs={"version":1,"appearance_mode":"dark","zoom_percent":100,"launch_minimized":False,"minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,"shortcuts":[]}
    try:
        g.fixture()
        profile={"version":1,"concurrent":True,"desktop":None,"seats":[{"id":"keyboard-minimum-a","name":"Keyboard Alpha","mouse":"virtual://mouse/1","keyboard":"virtual://keyboard/1","target_pid":g.target.pid,"cursor":{"lock_window":False,"keyboard":{"enabled":True,"focus":"clicked"}}}]}
        steps=[ui.step("start","Start routing"),ui.step("select","Select user 1"),ui.step("configure","Configure user"),ui.step("keyboard","Keyboard"),ui.step("checkbox","Keyboard enabled"),ui.step("apply","Apply cursor settings"),ui.step("home","Back to Home"),ui.step("stop","Stop routing")];assert len(steps)==8
        def verify(name,sample):
            strict.strict_equal(ui.read_json(g.preferences_path),prefs)
            if g.expected is None:g.expected=copy.deepcopy(sample["profile_state"]["profile"])
            want=copy.deepcopy(g.expected)
            if name in("checkbox","apply","home","stop"):want["seats"][0]["cursor"]["keyboard"]["enabled"]=False
            strict.strict_equal(sample["profile_state"]["profile"],want)
            if name in("start","select","configure"):
                if not g.minimum(name):return False
            if name=="start":
                rows=sample["state"]
                if not rows:return False
                assert rows[0]["keyboard"]["settings"]=={"enabled":True,"focus":"clicked"}
                g.layers.update(r["overlay"]["hwnd"]for r in rows)
                assert len(g.layers)==1 and all(h and ui.user32.IsWindow(h)for h in g.layers)
            if name=="keyboard":assert geometry.geometry(g.main_hwnd())["client_logical"]==[320.0,360.0]
            if name=="checkbox":
                if not g.frame():return False
            if name=="apply":
                rows=sample["state"]
                if rows[0]["keyboard"]["settings"]=={"enabled":True,"focus":"clicked"}:return False
                assert rows[0]["keyboard"]["settings"]=={"enabled":False,"focus":"clicked"}
                g.evidence["applied_state_after_original"]=rows
            if name=="stop":
                if any(ui.user32.IsWindow(h)for h in g.layers):return False
                recovery=ui.read_json(g.run/"ui.recovery.json")
                if recovery["phase"]!="stopped":return False
                assert recovery["can_start"]and not any(r["held_keys"]or r["buttons"]for r in g.probes())
                g.evidence["stop_recovery"]=recovery
            return True
        def after(gate):
            gate.close_target();gate.expected_exit=True
            assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0);gate.gui.wait(timeout=10)
            assert gate.gui.returncode==0 and not hotkeys.lifecycle_window(gate.gui.pid)
            return True
        archive=g.run/"runner-source";archive.mkdir()
        for name in(Path(__file__).name,"test-app-lifecycle-ui.py","test-keyboard-persistence-ui.py","owned-gui-geometry.py"):shutil.copy2(Path(__file__).with_name(name),archive/name)
        ui.put(archive/"all-script-dependency-hashes.json",{p.name:digest(p)for p in Path(__file__).parent.glob("*.py")})
        g.evidence.update(producer=str(args.producer),producer_sha256=digest(args.producer),retained_native_run=str(prior),retained_native_review_sha256=digest(prior/"independent-review.json"),scope="Missing minimumUI only:8GUI/oneprobe/0MCP/0packets/one320offdraftoriginal beforeApply")
        g.execute(profile,prefs,steps,verify,after_controls=after)
        g.evidence["result"]="PENDING INDEPENDENT MINIMUM PIXEL/DATA REVIEW"
    finally:
        try:g.close_target()
        except Exception as error:g.evidence.setdefault("cleanup_errors",[]).append(repr(error))
        g.close();lifecycle.remove_startup_key(g.startup_key)
        g.evidence["owned_exit_codes"]={str(p.pid):p.returncode for p in g.owned};g.evidence["known_overlays_closed"]=bool(g.layers)and all(not ui.user32.IsWindow(h)for h in g.layers)
        g.evidence["guid_value_absent"]=lifecycle.startup_value(g.startup_key)is None
        g.evidence["descriptor_absent"]=g.gui is None or not list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{g.gui.pid}-*.json"))
        pids=','.join(str(p.pid)for p in g.owned)
        audit=subprocess.run(["powershell","-NoProfile","-Command",f"@(Get-CimInstance Win32_Process | Where-Object {{ $_.ProcessId -in @({pids}) }} | Select-Object ProcessId,Name,ExecutablePath) | ConvertTo-Json -Compress"],capture_output=True,text=True,timeout=20)
        assert audit.returncode==0,audit.stderr
        g.evidence["scoped_cim_survivors"]=json.loads(audit.stdout)if audit.stdout.strip()else[]
        if g.evidence.get("cleanup_errors")or g.evidence["scoped_cim_survivors"]or not g.evidence["guid_value_absent"]or not g.evidence["descriptor_absent"]:g.evidence["result"]="NOT ACCEPTED"
        g.save()


def main():
    p=argparse.ArgumentParser();p.add_argument("--ready",action="store_true")
    for name in("executable","x86-executable","runtime","producer"):p.add_argument("--"+name,type=Path,required=True)
    p.add_argument("--producer-sha256",required=True);a=p.parse_args();producer,prior=freeze(a);workflow(a,producer,prior)


if __name__=="__main__":main()
