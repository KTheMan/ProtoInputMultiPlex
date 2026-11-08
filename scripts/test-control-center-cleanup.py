"""Finite independent compact-Home and navigation/lifecycle acceptance.

Only launch after root READY. Product screenshots and geometry belong to our
GUI PID; native input targets only our disposable virtual x64/x86 probes.
"""
import argparse
import copy
import importlib.util
from pathlib import Path
import time

def module(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value

maintenance = module("cleanup_maintenance", "test-profile-maintenance.py")
wheel = module("cleanup_wheel", "test-horizontal-wheel-ui.py")
geometry = module("cleanup_geometry", "owned-gui-geometry.py")
ui = maintenance.ui

class CleanupGate(maintenance.ProfileGate):
    def fixtures(self):
        self.args.assigned = False
        try: wheel.WheelUi.fixtures(self)
        except Exception as error:
            self.evidence["failure"] = repr(error)
            self.close()
            raise

    def before_first(self):
        if not self.offline or not getattr(self,"capture_initial_home",True): return
        if not (self.run/"ui.png").exists():return
        hwnd = geometry.owned_window(self.owned[-1].pid)
        if not hwnd: return
        shots = self.evidence.setdefault("initial_home", {})
        if "default" not in shots:
            observed = geometry.geometry(hwnd)
            assert all(abs(v-target) <= 2 for v, target in zip(observed["client_logical"], (420,600))), observed
            shot = geometry.copy_owned_product_frame(self.run/"ui.png", self.run/"home-default.png", hwnd)
            if not shot: return
            shots["default"] = shot
            self.minimum_after = time.time()
            geometry.resize_owned_client(hwnd, (360,480))
        if "minimum" not in shots:
            observed = geometry.geometry(hwnd)
            if any(abs(v-target)>2 for v,target in zip(observed["client_logical"],(360,480))): return
            shot = geometry.copy_owned_product_frame(self.run/"ui.png", self.run/"home-minimum.png", hwnd, self.minimum_after)
            if not shot: return
            shots["minimum"] = shot
            self.save()

    def viewport(self, expected):
        hwnd = geometry.owned_window(self.owned[-1].pid)
        if not hwnd: return None
        observed = geometry.geometry(hwnd)
        if any(abs(v-target)>2 for v,target in zip(observed["client_logical"],expected)): return None
        return observed

    def shot(self, name):
        if not hasattr(self,"shot_started"): self.shot_started = {}
        if name not in self.shot_started: self.shot_started[name] = time.time()
        hwnd = geometry.owned_window(self.owned[-1].pid)
        if not hwnd: return None
        return geometry.copy_owned_product_frame(self.run/"ui.png", self.run/(name+".png"), hwnd, self.shot_started[name])

    def agent(self):
        path = self.run/"ui.agent.json"
        return ui.read_json(path) if path.exists() else None

def offline(args):
    gate = CleanupGate(args, "control-center-visual-tail" if args.visual_only else "control-center-offline", offline=True, count=0)
    profile = {"version":1,"concurrent":True,"seats":[{"id":f"compact-own-{i}",
        "name":("Owned FIRST long name 東京🙂 "+"W"*90) if i==0 else "Owned LAST 12" if i==11 else f"Owned user {i+1}",
        "mouse":f"virtual://mouse/{i+1}","keyboard":f"virtual://keyboard/{i+1}","target_pid":991000+i} for i in range(12)]}
    step=ui.step
    steps=[step("select-first","Select user 1"),step("configure-first","Configure user"),step("back-first","Back to Home"),
        step("select-last","Select user 12"),step("configure-last","Configure user"),step("back-last","Back to Home"),
        step("settings-profiles","Settings"),step("profiles","Profiles & backend"),step("home-profiles","Back to Home"),
        step("settings-lab","Settings"),step("lab","Input lab"),step("home-lab","Back to Home"),
        step("settings-agent","Settings"),step("agent","Agent Access"),step("home-agent","Back to Home")]
    steps[0]["wait_ms"]=12000
    if args.visual_only:steps=steps[:1]
    baseline=None
    def verify(name,sample):
        nonlocal baseline
        snapshot=gate.snapshot()
        assert not (gate.run/"ui.state.json").exists(),"Navigation started a routing worker"
        if name=="select-first":
            assert set(gate.evidence.get("initial_home",{}))=={"default","minimum"},"Initial real Home captures missing"
            baseline=copy.deepcopy(snapshot["profile"])
            review_path=gate.run/"home.review.json"
            if not review_path.exists():return False
            review=ui.read_json(review_path)
            assert review["accepted"],review
            gate.evidence["independent_home_visual_review"]=review
        assert snapshot["profile"]==baseline,"Navigation changed complete profile"
        expected=(360,480) if name=="select-first" else (420,600) if name.startswith("back-") or name.startswith("home-") or name=="select-last" else (900,720)
        size=gate.viewport(expected)
        if not size: return False
        sample["viewport"]=size
        if name=="configure-last":
            shot=gate.shot("selected-user-12")
            if not shot:return False
            sample["selection_image_requires_independent_review"]=shot
        if name=="agent":
            policy=gate.agent()
            if policy is None:return False
            assert not policy["policy_enabled"] and not policy["capture"] and not policy["session_control"]
            assert not policy["clients"] and not policy["pending"]
            gate.evidence["agent_navigation_baseline"]=policy
        if name=="home-agent":assert gate.agent()==gate.evidence["agent_navigation_baseline"]
        return True
    try:
        gate.execute(profile,steps,verify)
        return gate.run
    finally:gate.close()

def live(args):
    gate=CleanupGate(args,"control-center-stop-tail" if args.stop_tail else "control-center-live",offline=False,count=2)
    gate.fixtures()
    profile={"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":gate.pids},
        "seats":[{"id":f"compact-live-{i}","name":f"Owned live user {i+1}","mouse":f"virtual://mouse/{i+1}",
            "keyboard":f"virtual://keyboard/{i+1}","desktop_start":gate.points[i]}for i in range(2)]}
    step=ui.step
    steps=[step("start","Start routing"),step("settings-agent-before","Settings"),step("agent-before","Agent Access"),
        step("home-before","Back to Home"),step("settings-lab","Settings"),step("lab","Input lab"),
        step("focus-a","1 Hold click"),step("focus-a-up","1 Release click"),
        step("focus-b","2 Hold click"),step("focus-b-up","2 Release click"),
        step("hold-a","Hold key",{"selected_user":0,"key_name":"f5"}),step("hold-b","Hold key",{"selected_user":1,"key_name":"f6"}),
        step("home-held","Back to Home"),step("settings-agent-held","Settings"),step("agent-held","Agent Access"),
        step("home-pause","Back to Home"),step("pause","Pause"),step("resume","Resume"),
        step("settings-rehold","Settings"),step("lab-rehold","Input lab"),
        step("rehold-a","Hold key",{"selected_user":0,"key_name":"f5"}),step("rehold-b","Hold key",{"selected_user":1,"key_name":"f6"}),
        step("home-stop","Back to Home"),step("stop","Stop routing")]
    if args.stop_tail:
        keep={"start","settings-lab","lab","focus-a","focus-a-up","focus-b","focus-b-up","hold-a","hold-b","home-stop","stop"}
        steps=[s for s in steps if s["id"] in keep]
    overlays=[];baseline=None;policy=None
    def key_events(probes,index):
        return [(e["vk"],e["pressed"])for e in probes[index]["key_events"]]
    def verify(name,sample):
        nonlocal overlays,baseline,policy
        state,probes=sample["state"],sample["probes"]
        snapshot=gate.snapshot()
        if name=="start":
            if len(state)!=2:return False
            overlays=[u["overlay"]["hwnd"]for u in state]
            baseline=copy.deepcopy(snapshot["profile"])
        assert snapshot["profile"]==baseline,"Navigation/lifecycle changed full profile"
        if name in ("agent-before","agent-held"):
            current=gate.agent()
            if current is None:return False
            assert not current["policy_enabled"] and not current["capture"] and not current["session_control"] and not current["clients"]
            if policy is None:policy=current
            else:assert current==policy,"Navigation changed Agent Access grants"
        if name not in ("start","stop"):
            assert [u["overlay"]["hwnd"]for u in state]==overlays,"Navigation/Resume replaced session overlays"
        if name=="focus-a" and not probes[0]["buttons"]&1:return False
        if name=="focus-b" and not probes[1]["buttons"]&1:return False
        if name in ("hold-a","rehold-a"):
            if 116 not in probes[0]["held_keys"]:return False
        if name in ("hold-b","rehold-b","home-held","settings-agent-held","agent-held","home-pause","home-stop"):
            if not (116 in probes[0]["held_keys"] and 117 in probes[1]["held_keys"]):return False
        if name=="home-stop":
            shot=gate.shot("home-live")
            if not shot:return False
            sample["owned_home_image_requires_independent_review"]=shot
        if name in ("pause","resume"):
            if any(probes[i]["buttons"] or vk in probes[i]["held_keys"] for i,vk in enumerate((116,117))):return False
            assert key_events(probes,0)==[(116,True),(116,False)]
            assert key_events(probes,1)==[(117,True),(117,False)]
        if name=="stop":
            if any(ui.user32.IsWindow(h)for h in overlays):return False
            loops=1 if args.stop_tail else 2
            expected_a=[(116,True),(116,False)]*loops;expected_b=[(117,True),(117,False)]*loops
            if len(key_events(probes,0))<2*loops or len(key_events(probes,1))<2*loops:return False
            assert key_events(probes,0)==expected_a and key_events(probes,1)==expected_b,"Stop did not deliver plain paired UP"
        if name.startswith("home-") or name in ("start","pause","resume","stop"):
            observed=gate.viewport((420,600))
            if not observed:return False
            sample["viewport"]=observed
        return True
    try:
        gate.execute(profile,steps,verify)
        return gate.run
    finally:gate.close()

def advanced(args):
    gate=CleanupGate(args,"control-center-routing-options",offline=True,count=0)
    gate.capture_initial_home=False
    profile={"version":1,"concurrent":True,"desktop":None,"seats":[
        {"id":f"compact-options-{i}","name":f"Owned options user {i+1}","mouse":f"virtual://mouse/{i+1}",
         "keyboard":f"virtual://keyboard/{i+1}","target_pid":991000+i,"cursor":{"lock_window":False}}for i in range(2)]}
    steps=[ui.step("options","Routing options"),ui.step("automatic-on","Automatic window targeting"),
           ui.step("automatic-off","Automatic window targeting"),ui.step("collapse","Routing options")]
    baseline=None
    def verify(name,sample):
        nonlocal baseline
        assert not (gate.run/"ui.state.json").exists(),"Offline routing options started a worker"
        current=gate.snapshot()["profile"]
        if name=="options":
            assert current.get("desktop") is None
            assert current["concurrent"] and all(not s["cursor"]["lock_window"]for s in current["seats"])
            baseline=copy.deepcopy(current)
        if name=="automatic-on":
            assert current["desktop"]=={"mode":"switched","multi_keyboard":True,"allowed_pids":[]}
            expected=copy.deepcopy(baseline);expected["desktop"]=current["desktop"]
            assert current==expected,"Automatic ON changed unrelated complete profile fields"
            shot=gate.shot("routing-options-on")
            if not shot:return False
            sample["owned_options_image_requires_independent_review"]=shot
        if name in ("automatic-off","collapse"):assert current==baseline,"Automatic OFF did not preserve complete original profile"
        if name=="collapse":
            shot=gate.shot("home-current")
            if not shot:return False
            sample["owned_current_home_image_requires_independent_review"]=shot
        return True
    try:
        gate.execute(profile,steps,verify)
        return gate.run
    finally:gate.close()

def main():
    parser=argparse.ArgumentParser()
    for name in ("executable","x86-executable","runtime"):parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--ready",action="store_true")
    parser.add_argument("--phase",choices=("offline","live","advanced"),default="offline")
    parser.add_argument("--visual-only",action="store_true")
    parser.add_argument("--stop-tail",action="store_true")
    args=parser.parse_args();assert args.ready,"Wait for root READY/exclusive slot"
    assert not args.visual_only or args.phase=="offline"
    assert not args.stop_tail or args.phase=="live"
    geometry.physical_pixel_context()
    {"offline":offline,"live":live,"advanced":advanced}[args.phase](args)

if __name__=="__main__":main()
