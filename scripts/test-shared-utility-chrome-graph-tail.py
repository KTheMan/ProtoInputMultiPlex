"""Graph-only shared utility chrome pixel continuation; frozen READY required."""
import argparse
import copy
import hashlib
import importlib.util
from pathlib import Path
import shutil
import time


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def workflow(args):
    lifecycle = module("chrome_lifecycle", "test-app-lifecycle-ui.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()

    class Gate(Base):
        def __init__(self):
            super().__init__(args,"shared-utility-chrome-graph-tail",offline=True,count=0)
            self.frames,self.started={},{}
            self.normalized=False;self.normalize_started=None

        def frame(self,name,size):
            if name in self.frames:return True
            hwnd=self.main_hwnd()
            if name not in self.started:
                geometry.resize_owned_client(hwnd,size);self.started[name]=time.time();return False
            current=geometry.geometry(hwnd)
            if any(abs(a-b)>2 for a,b in zip(current["client_logical"],size)):return False
            frame=geometry.copy_owned_product_frame(self.run/"ui.png",self.run/(name+".png"),hwnd,self.started[name])
            if not frame:return False
            frame["geometry"]=current;self.frames[name]=frame;return True

        def graph(self):
            return ui.read_json(self.run/"ui.graph.json")

        def normalize_scroll_extent(self):
            if self.normalized:return True
            hwnd=self.main_hwnd()
            if self.normalize_started is None:
                geometry.resize_owned_client(hwnd,[860,1200])
                self.normalize_started=time.time();return False
            current=geometry.geometry(hwnd)
            if any(abs(a-b)>2 for a,b in zip(current["client_logical"],[860,1200])):return False
            source=self.run/"ui.png"
            if source.stat().st_mtime<=self.normalize_started:return False
            from PIL import Image
            with Image.open(source)as image:
                image.load()
                if list(image.size)!=current["client_pixels"]:return False
            if time.time()-self.normalize_started<.6:return False
            self.evidence["ordinary_resize_normalization"]={"temporary_geometry":current,
                "fresh_product_png_pixels":list(image.size),"settle_seconds":time.time()-self.normalize_started,
                "scope":"Ordinary owned-window resize allows natural ScrollArea extent clamp, no synthetic scroll/input/state."}
            self.save();self.normalized=True;return True

    prefix_path=args.from_run/"evidence.json"
    review_path=args.from_run/"independent-visual-review.json"
    assert args.from_run.name=="acceptance-graph-shared-utility-chrome-20261015-015944"
    assert hashlib.sha256(prefix_path.read_bytes()).hexdigest()=="cb7054003c97f10ec8bc5973e40d22a7f5e7233e93dad3585b94f404956cc419"
    assert hashlib.sha256(review_path.read_bytes()).hexdigest()=="f7995f13766a4df45e05212d109e23d7d08d362224f9e61ac5260eb6b16625d9"
    producer=Path("runtime/producer-shared-utility-chrome-20261015-015712/verification.json")
    manifest=ui.read_json(producer)["source_after_sha256"]
    assert len(manifest)==64 and all(hashlib.sha256(Path(p).read_bytes()).hexdigest()==v for p,v in manifest.items())
    assert hashlib.sha256(args.executable.read_bytes()).hexdigest()=="8988f3d15e2512f84f16f5de4edc986519959bc1b9cffd803202541a9dcbe5a7"
    assert hashlib.sha256(args.x86_executable.read_bytes()).hexdigest()=="554fb46e006597bdb955df525ce1e774acd57c012823fddd0ef5dc3c13df8b52"
    g=Gate()
    profile={"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":[987301,987302]},
        "seats":[{"id":f"chrome-owned-{i}","name":"Owned Alpha" if i==0 else "Owned Beta",
            "mouse":f"virtual://mouse/{i+1}","keyboard":f"virtual://keyboard/{i+1}",
            "cursor":{"color":[70,190,160] if i==0 else [230,140,60]}}for i in range(2)]}
    prefs={"version":1,"appearance_mode":"dark","zoom_percent":100,"launch_minimized":False,
        "minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,"shortcuts":[]}
    step=ui.step
    fields={"graph_source":"chrome-owned-0","graph_target":"chrome-owned-1",
        "graph_mouse":"true","graph_keyboard":"false","graph_text":"false"}
    steps=[step("settings","Settings"),step("automation-category","Settings category Automation"),
        step("graph","Replication Graph"),step("queue-open","Queue diagnostics"),
        step("queue-close","Queue diagnostics"),step("connect-details","Connect users"),
        step("connect","Connect selected users",fields),step("duplicate","Connect selected users"),
        step("home-graph","Back to Home")]
    assert len(steps)==9
    expected=None;preferences_bytes=None;draft=None

    def verify(name,sample):
        nonlocal expected,preferences_bytes,draft
        current=g.snapshot()["profile"]
        if expected is None:
            expected=copy.deepcopy(current);preferences_bytes=g.preferences_path.read_bytes()
        assert current==expected,"Chrome/navigation mutated the complete profile"
        assert g.preferences_path.read_bytes()==preferences_bytes,"Chrome/navigation mutated preferences"
        assert not(g.run/"ui.state.json").exists(),"Stopped gate unexpectedly created a routing worker"
        if name=="queue-open":
            state=g.graph();assert not state["pending"] and not state["draft"]["edges"] and not state["applied"]["edges"]
            if not g.frame("graph-queue-open-normal",[860,900]):return False
        if name=="queue-close":
            state=g.graph();assert not state["pending"] and not state["draft"]["edges"] and not state["applied"]["edges"]
        if name=="connect":
            state=g.graph()
            if len(state["draft"]["edges"])!=1:return False
            assert not state["applied"]["edges"] and not state["pending"] and not state["error"]
            edge=state["draft"]["edges"][0]
            assert edge["source"]=="chrome-owned-0" and edge["target"]=="chrome-owned-1"
            assert edge["mouse"] and not edge["keyboard"] and not edge["text"]
            draft=copy.deepcopy(state["draft"])
        if name=="duplicate":
            state=g.graph()
            if not state["error"]:return False
            assert state["draft"]==draft and not state["applied"]["edges"] and not state["pending"]
            sample["preserved_draft_with_real_duplicate_error"]=state
            if not g.normalize_scroll_extent():return False
            if not g.frame("graph-closed-diagnostics-error-minimum",[320,360]):return False
            g.evidence["ordinary_resize_normalization"]["final_geometry"]=g.frames["graph-closed-diagnostics-error-minimum"]["geometry"]
        sample["full_profile_preferences_unchanged"]=True
        return True

    def after(gate):
        assert gate.snapshot()["profile"]==expected
        assert gate.preferences_path.read_bytes()==preferences_bytes
        ui.put(gate.run/"final-profile.json",expected)
        gate.expected_exit=True
        assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0)
        gate.gui.wait(timeout=10)
        assert gate.gui.returncode==0 and not hotkeys.lifecycle_window(gate.gui.pid)
        return True

    try:
        provenance=g.run/"runner-source";provenance.mkdir()
        shutil.copy2(Path(__file__),provenance/Path(__file__).name)
        g.evidence["retained_prefix"]={"path":str(prefix_path),"sha256":hashlib.sha256(prefix_path.read_bytes()).hexdigest(),"preserved_status":ui.read_json(prefix_path)["result"],"accepted_receipts":16,"review":str(review_path)}
        g.evidence["scope"]="Only missing Graph diagnostics/error pixels after ordinary window resize; no automatic scroll-to-error/max-reachability claim. No routing/worker/targets/MCP/Agent. Prior chrome and16control outcomes retained."
        g.execute(profile,prefs,steps,verify,lambda gate:gate.main_hwnd()is not None,after)
        assert len(g.frames)==2
        g.evidence.update(result="PENDING INDEPENDENT VISUAL REVIEW",control_profile_result="PASS",captures=g.frames)
        g.save();return g.run
    finally:
        g.close();lifecycle.remove_startup_key(g.startup_key)


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument("--ready",action="store_true")
    parser.add_argument("--from-run",type=Path,required=True)
    for name in("executable","x86-executable","runtime"):parser.add_argument("--"+name,type=Path,required=True)
    args=parser.parse_args();assert args.ready,"Await final source/producer/exclusive ROOT READY"
    workflow(args)


if __name__=="__main__":main()
