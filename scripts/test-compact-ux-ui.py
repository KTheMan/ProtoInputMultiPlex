"""Finite independent owned-window compact Home/form visual gate.

No app/native imports before --ready. Reviews must inspect the saved originals;
geometry alone never produces an accepted visual judgment.
"""
import argparse
import copy
import importlib.util
from pathlib import Path
import shutil
import time

def module(name,file):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(file))
    result=importlib.util.module_from_spec(spec);spec.loader.exec_module(result)
    return result

def workflow(args):
    lifecycle=module("compact_lifecycle_driver","test-app-lifecycle-ui.py")
    Base,ui,hotkeys,geometry=lifecycle.gate_type()
    long=args.phase=="long-forms"

    class CompactGate(Base):
        def __init__(self):
            super().__init__(args,"compact-ux-"+args.phase,offline=True,count=0)
            self.captures={};self.capture_after={};self.manual_frames=[]

        def viewport(self,expected):
            hwnd=self.main_hwnd()
            if not hwnd:return None
            observed=geometry.geometry(hwnd)
            if any(abs(a-b)>2 for a,b in zip(observed["client_logical"],expected)):return None
            return observed

        def capture(self,name,expected,after=0):
            self.capture_after.setdefault(name,time.time())
            observed=self.viewport(expected)
            if not observed:return None
            shot=geometry.copy_owned_product_frame(self.run/"ui.png",self.run/(name+".png"),observed["hwnd"],max(after,self.capture_after[name]))
            if shot:self.captures[name]=shot;self.evidence["owned_images"]=self.captures;self.save()
            return shot

        def resize(self,size):
            hwnd=self.main_hwnd();assert hwnd
            geometry.resize_owned_client(hwnd,size)
            return time.time()

        def review(self,name,required):
            # Finish finite app interactions and close first. Independent model
            # review of immutable originals is a separate acceptance decision.
            self.evidence.setdefault("required_visual_reviews",{})[name]=list(required)
            return True

    gate=CompactGate()
    gate.evidence["architecture_scope"]={"GUI":"current frozen x64 image","x86_copy_launched":False,
        "x86_metadata":"historical unused compatibility argument; not current i686 build proof"if args.unused_historical_x86 else"copied but unused in this offline phase"}
    count=12 if long else 2
    names=["Owned Alpha","Owned Beta"]if not long else[
        "Owned long first 東京🙂 "+"WideName "*12,*[f"Owned user {i}"for i in range(2,12)],"Owned LAST 12"]
    # Exact original product palette from gui.rs / pinned Ouroboros core tokens.
    palette=((96,165,250),(239,68,68),(34,197,94),(94,234,212),(245,158,11),(212,212,216))
    profile={"version":1,"concurrent":True,"seats":[{"id":f"compact-owned-{i}","name":names[i],
        "mouse":f"virtual://mouse/{i+1}","keyboard":None if i==2 else f"virtual://keyboard/{i+1}","target_pid":987001+i,
        "cursor":{"color":list(palette[i%len(palette)])}}for i in range(count)]}
    prefs={"version":1,"launch_minimized":False,"minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,
        "shortcuts":[{"action":action,"key":key,"modifiers":{"control":True,"alt":True,"shift":True,"windows":False}}
            for action,key in(("stop","f11"),("pause_resume","f6"))]}
    step=ui.step
    steps=[step("first","Select user 1"),step("last",f"Select user {count}"),
        step("configure","Configure user"),step("back","Back to Home")]
    if long:
        steps += [step("settings","Settings"),step("preferences","Application preferences"),
            step("stop-off","Shortcut Stop routing"),step("stop-on","Shortcut Stop routing"),
            step("invalid","Apply application preferences",{"app_shortcut_Stop":"F11"},disabled=True),
            step("revert","Revert application preferences"),step("apply","Apply application preferences"),step("home","Back to Home")]
    steps[0]["wait_ms"]=12000
    baseline=None;form_stage=0;form_after=0

    def before(g):
        nonlocal baseline
        default=(360,540)if long else(360,360)
        if "home-default"not in g.captures:
            if not g.capture("home-default",default):return False
            baseline=copy.deepcopy(g.snapshot()["profile"])
            g.capture_after["next"]=g.resize((320,360)if long else(450,410))
        if not long and len(g.manual_frames)<2:
            if not g.viewport((450,410)):return False
            stamp=(g.run/"ui.png").stat().st_mtime if(g.run/"ui.png").exists()else 0
            after=max([g.capture_after["next"]]+g.manual_frames)
            if stamp<=after:return False
            if not g.capture("home-manual-repaint-"+str(len(g.manual_frames)+1),(450,410),after):return False
            g.manual_frames.append(stamp)
            if len(g.manual_frames)<2:return False
            g.capture_after["next"]=g.resize((320,360))
        if "home-minimum"not in g.captures:
            if not g.capture("home-minimum",(320,360),g.capture_after["next"]):return False
        return g.review("home",("home-default.png","home-minimum.png"))

    def verify(name,sample):
        nonlocal form_stage,form_after
        assert not(gate.run/"ui.state.json").exists(),"Offline layout navigation started a routing worker"
        assert sample["profile_state"]["profile"]==baseline,"Layout navigation changed stored full names/profile values"
        if name=="last":
            shot=gate.capture("selected-last-minimum",(320,360))
            if not shot:return False
            sample["selected_last_image"]=shot
        if name=="configure":
            shot=gate.capture("configure-selected",(680,620))
            if not shot:return False
            sample["selected_configuration_image"]=shot
        if name=="back":
            expected=(360,540)if long else(360,360)
            shot=gate.capture("home-back-selection",expected)
            if not shot:return False
            sample["selection_preserved_image"]=shot
            if not long:return gate.review("selection",("selected-last-minimum.png","configure-selected.png","home-back-selection.png"))
        if name=="settings":
            shot=gate.capture("settings-grouping",(680,620))
            if not shot:return False
            sample["settings_grouping_image"]=shot
        if name=="stop-off":
            if form_stage==0:
                if not gate.capture("preferences-aligned",(680,620)):return False
                form_after=gate.resize((320,360));form_stage=1
            if form_stage==1:
                if not gate.capture("preferences-stacked",(320,360),form_after):return False
                form_stage=2
            return gate.review("forms",("settings-grouping.png","preferences-aligned.png","preferences-stacked.png"))
        if name=="invalid":
            assert ui.read_json(gate.preferences_path)==prefs
            if not gate.capture("preferences-invalid-footer",(320,360)):return False
            return gate.review("validation",("preferences-invalid-footer.png",))
        if name=="apply":
            status=sample["profile_state"]["status"]
            if "applied"not in status.lower():return False
            assert "not applied"not in status.lower(),status
            assert ui.read_json(gate.preferences_path)==prefs
            assert gate.app_snapshot("ui.app-preferences.json")["committed"]==prefs
        if name=="home":
            if not gate.capture("home-final",(360,540)):return False
        return True

    def after(g):
        g.expected_exit=True;hwnd=g.main_hwnd();assert hwnd
        assert geometry.user32.PostMessageW(hwnd,0x10,0,0)
        g.gui.wait(timeout=10);assert g.gui.returncode==0
        assert not hotkeys.lifecycle_window(g.gui.pid)
        return True

    try:
        snapshot=gate.run/"runner-source";snapshot.mkdir()
        shutil.copy2(Path(__file__),snapshot/Path(__file__).name)
        gate.execute(profile,prefs,steps,verify,before,after)
        gate.evidence["control_and_geometry_result"]="PASS"
        gate.evidence["result"]="PENDING INDEPENDENT VISUAL REVIEW"
        gate.save()
        return gate.run
    finally:gate.close();lifecycle.remove_startup_key(gate.startup_key)

def main():
    parser=argparse.ArgumentParser()
    for name in("executable","x86-executable","runtime"):parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--phase",choices=("short","long-forms"),required=True)
    parser.add_argument("--ready",action="store_true")
    parser.add_argument("--unused-historical-x86",action="store_true")
    args=parser.parse_args();assert args.ready,"Await combined producer READY/exclusive native slot"
    workflow(args)

if __name__=="__main__":main()
