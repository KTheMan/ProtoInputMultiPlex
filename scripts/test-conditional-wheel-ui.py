"""Actual conditional wheel dropdown/apply/persistence and unavailable controls.

Only root READY permits launch. Every native target and rendered screenshot is
owned by this runner; selection comes from actual Ouroboros MenuItem clicks.
"""
import argparse
import copy
import importlib.util
import json
from pathlib import Path
import time
from PIL import Image

def module(name,file):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(file))
    value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value

maintenance=module("conditional_maintenance_ui","test-profile-maintenance.py")
wheel_ui=module("conditional_old_wheel_ui","test-horizontal-wheel-ui.py")
ui=maintenance.ui

class ConditionalUi(maintenance.ProfileGate):
    def fixtures(self):wheel_ui.WheelUi.fixtures(self)

def live(args):
    gate=ConditionalUi(args,"conditional-wheel-assigned-tail" if args.cleanup_tail else "conditional-wheel-assigned" if args.assigned else "conditional-wheel-ui",offline=False,count=2)
    gate.fixtures()
    profile={"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":gate.pids},
      "seats":[{"id":f"conditional-own-{i}","name":f"Owned conditional wheel {i+1}","mouse":f"virtual://mouse/{i+1}","keyboard":f"virtual://keyboard/{i+1}",
        "desktop_start":gate.points[i],"cursor":{"lock_window":args.assigned,"wheel_scale":.5 if i==0 else 1,"invert_wheel":i==0}} for i in range(2)]}
    if args.assigned:
        profile["desktop"]=None
        for i,seat in enumerate(profile["seats"]):seat["target_pid"]=gate.pids[i]
    step=ui.step;steps=[step("lab","Input lab"),step("start","Start routing")]
    if args.assigned:steps += [step("assigned-anchor","1 Left"),step("assigned-focus","1 Hold click"),step("assigned-focus-up","1 Release click")]
    else:
        for i in range(2):steps += [step(f"select-{i}",f"{i+1} Hold click"),step(f"drop-{i}",f"{i+1} Release click")]
    steps += [step("other-owner","Hold key",{"selected_user":1,"key_name":"f6"}),step("configure","Configure user",{"selected_user":0}),step("wheel-page","Wheel"),
      step("open-selector","Horizontal emulation selector"),step("choose-back","While Back is down")]
    if not args.assigned:steps += [step("dropdown-image","Horizontal emulation selector"),step("choose-back-confirm","While Back is down")]
    steps += [step("apply","Apply cursor settings",{"horizontal_wheel_scale":2,"invert_horizontal_wheel":"true"}),
      step("input-page","Input lab"),step("raw-back-down","1 Hold Back"),step("converted-vertical","1 Wheel"),
      step("raw-back-up","1 Release Back"),step("ordinary-vertical","1 Wheel"),step("incoming-horizontal-ignore","1 Horizontal wheel"),
      step("other-owner-up","Release key",{"selected_user":1,"key_name":"f6"}),step("stop","Stop routing")]
    if not args.assigned:steps += [step("profiles","Profiles"),step("save","Save profile",{"profile_path":gate.run/"saved.json"}),
      step("reload","Load profile",{"profile_path":gate.run/"saved.json"}),step("roundtrip","Save profile",{"profile_path":gate.run/"roundtrip.json"})]
    if args.cleanup_tail:
        profile["seats"][0]["cursor"].update(horizontal_wheel_emulation="while_back",horizontal_wheel_scale=2,invert_horizontal_wheel=True)
        indexed={entry["id"]:entry for entry in steps}
        steps=[indexed["lab"],indexed["start"],step("focus-b-child","2 Hold click"),step("focus-b-up","2 Release click")]
        steps += [indexed[name] for name in ("other-owner","configure","wheel-page","apply","input-page","other-owner-up","stop")]
    baseline=None;overlays=[];image_wall=None
    def verify(name,sample):
        nonlocal baseline,overlays,image_wall
        reports,state=sample["probes"],sample["state"]
        if name=="start":
            if len(state)!=2:return False
            overlays=[u["overlay"]["hwnd"] for u in state]
        if name.startswith("select-") and not reports[int(name[-1])]["buttons"]&1:return False
        if name.startswith("drop-") and reports[int(name[-1])]["buttons"]:return False
        if name=="assigned-anchor":
            if reports[0]["cursor"]!=gate.points[0]:return False
        if name=="assigned-focus" and not reports[0]["buttons"]&1:return False
        if name=="assigned-focus-up" and reports[0]["buttons"]:return False
        if name=="focus-b-child" and not reports[1]["buttons"]&1:return False
        if name=="focus-b-up" and reports[1]["buttons"]:return False
        if name=="other-owner":
            if 117 not in reports[1]["held_keys"]:return False
            if args.cleanup_tail and not reports[1]["key_events"]:return False
            if args.cleanup_tail:assert [(e["vk"],e["pressed"]) for e in reports[1]["key_events"]]==[(117,True)]
        if name in ("choose-back","choose-back-confirm","dropdown-image","apply"):
            assert gate.snapshot()["profile"]["seats"][0]["cursor"]["horizontal_wheel_emulation"]=="while_back"
        if name=="dropdown-image":
            image_wall=image_wall or time.time();png=gate.run/"ui.png"
            if not png.exists() or png.stat().st_mtime<image_wall:return False
            with Image.open(png) as image:image.load();image.save(gate.run/"conditional-wheel-controls.png")
            sample["image_requires_independent_review"]=str(gate.run/"conditional-wheel-controls.png")
        if name=="apply":assert 117 in reports[1]["held_keys"]
        if name=="input-page":baseline=copy.deepcopy(reports)
        if name=="raw-back-down" and reports[0]["buttons"]!=8:return False
        if name=="converted-vertical":
            if reports[0]["horizontal_wheel_events"]!=baseline[0]["horizontal_wheel_events"]+1:return False
            event=next(e for e in reversed(reports[0]["native_events"]) if e["kind"]=="horizontal_wheel")
            assert event["delta"]==-240 and event["wheel_screen"]==gate.points[0] and event["wheel_modifiers"]==0x20,event
            assert reports[0]["wheel_events"]==baseline[0]["wheel_events"]
            assert reports[1]["horizontal_wheel_events"]==baseline[1]["horizontal_wheel_events"] and 117 in reports[1]["held_keys"]
        if name=="raw-back-up" and reports[0]["buttons"]:return False
        if name=="ordinary-vertical":
            if reports[0]["wheel_events"]!=baseline[0]["wheel_events"]+1:return False
            event=next(e for e in reversed(reports[0]["native_events"]) if e["kind"]=="wheel")
            assert event["delta"]==-60 and event["wheel_screen"]==gate.points[0],event
            assert reports[0]["horizontal_wheel_events"]==baseline[0]["horizontal_wheel_events"]+1
        if name=="incoming-horizontal-ignore":
            assert reports[0]["wheel_events"]==baseline[0]["wheel_events"]+1 and reports[0]["horizontal_wheel_events"]==baseline[0]["horizontal_wheel_events"]+1
            assert reports[1]["wheel_events"]==baseline[1]["wheel_events"] and 117 in reports[1]["held_keys"]
        if name=="other-owner-up":
            if 117 in reports[1]["held_keys"]:return False
            if len(reports[1]["key_events"])<2:return False
            assert [(e["vk"],e["pressed"]) for e in reports[1]["key_events"]]==[(117,True),(117,False)]
        if name=="stop":
            if any(r["held_keys"] or r["buttons"] for r in reports) or any(ui.user32.IsWindow(h) for h in overlays):return False
        if name in ("save","roundtrip"):
            path=gate.run/("saved.json" if name=="save" else "roundtrip.json")
            saved=ui.read_json(path);cursor=saved["seats"][0]["cursor"]
            assert cursor["horizontal_wheel_emulation"]=="while_back" and cursor["horizontal_wheel"]=="ignore"
            assert cursor["horizontal_wheel_scale"]==2 and cursor["invert_horizontal_wheel"] and cursor["wheel_scale"]==.5 and cursor["invert_wheel"]
            assert saved["seats"][1]["cursor"]["horizontal_wheel_emulation"]=="never"
            if name=="roundtrip":assert saved==ui.read_json(gate.run/"saved.json")
        return True
    try:gate.execute(profile,steps,verify);return gate.run
    finally:gate.close()

def offline(args):
    gate=ConditionalUi(args,"conditional-wheel-fixed-tail" if args.fixed_tail else "conditional-wheel-unavailable",offline=True,count=0)
    physical={"version":1,"concurrent":True,"seats":[{"id":"conditional-physical","name":"Owned unresolved physical","mouse":"\\\\?\\HID#ConditionalAcceptance_NotADevice","keyboard":None,"target_pid":991000}]}
    fixed={"version":1,"concurrent":False,"seats":[{"id":"conditional-fixed","name":"Owned fixed virtual","mouse":"virtual://mouse/71","keyboard":"virtual://keyboard/71","target_pid":991001}]}
    invalid_physical=copy.deepcopy(physical);invalid_physical["seats"][0]["cursor"]={"horizontal_wheel_emulation":"always"}
    invalid_fixed=copy.deepcopy(fixed);invalid_fixed["seats"][0]["cursor"]={"horizontal_wheel_emulation":"always"}
    invalid_physical["seats"][0]["name"]="Rejected physical candidate"
    invalid_fixed["seats"][0]["name"]="Rejected fixed candidate"
    for filename,value in (("fixed.json",fixed),("invalid-physical.json",invalid_physical),("invalid-fixed.json",invalid_fixed)):ui.put(gate.run/filename,value)
    step=ui.step
    steps=[step("physical-configure","Configure user",{"selected_user":0}),step("physical-wheel","Wheel"),
      step("physical-disabled","Horizontal emulation selector",disabled=True),step("physical-profiles","Profiles"),
      step("physical-invalid-load","Load profile",{"profile_path":gate.run/"invalid-physical.json"}),step("physical-save","Save profile",{"profile_path":gate.run/"physical-retained.json"}),
      step("fixed-load","Load profile",{"profile_path":gate.run/"fixed.json"}),step("fixed-configure","Configure user",{"selected_user":0}),step("fixed-wheel","Wheel"),
      step("fixed-disabled","Horizontal emulation selector",disabled=True),step("fixed-profiles","Profiles"),
      step("fixed-invalid-load","Load profile",{"profile_path":gate.run/"invalid-fixed.json"}),step("fixed-save","Save profile",{"profile_path":gate.run/"fixed-retained.json"})]
    if args.fixed_tail:steps=[entry for entry in steps if entry["id"] in ("fixed-configure","fixed-wheel","fixed-disabled","fixed-profiles","fixed-invalid-load","fixed-save")]
    expected=None
    def verify(name,sample):
        nonlocal expected
        assert not (gate.run/"ui.state.json").exists(),"Unavailable-control gate unexpectedly started routing"
        if name in ("physical-configure","fixed-load") or args.fixed_tail and name=="fixed-configure":expected=gate.snapshot()["profile"]
        if name in ("physical-save","fixed-save"):
            fixture=physical if name=="physical-save" else fixed
            saved=ui.read_json(gate.run/("physical-retained.json" if name=="physical-save" else "fixed-retained.json"))
            assert saved["seats"][0]["id"]==fixture["seats"][0]["id"] and saved["seats"][0]["name"]==fixture["seats"][0]["name"]
            assert saved["seats"][0]["cursor"]["horizontal_wheel_emulation"]=="never"
            assert saved==expected
            sample["retained_profile"]=saved
        if name.endswith("invalid-load"):
            status=gate.snapshot()["status"].lower();assert "emulat" in status and ("virtual" in status or "concurrent" in status),status
            assert gate.snapshot()["profile"]==expected
        return True
    try:gate.execute(fixed if args.fixed_tail else physical,steps,verify);return gate.run
    finally:gate.close()

def main():
    parser=argparse.ArgumentParser()
    for name in ("executable","x86-executable","runtime"):parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--ready",action="store_true");parser.add_argument("--phase",choices=("automatic","assigned","offline"),default="automatic")
    parser.add_argument("--cleanup-tail",action="store_true",help="Assigned child-focus/other-owner/Stop tail after wheel prefix")
    parser.add_argument("--fixed-tail",action="store_true",help="Only fresh fixed-mode unavailable/preservation criterion")
    args=parser.parse_args();assert args.ready,"Wait for root READY/exclusive slot"
    args.assigned=args.phase=="assigned"
    assert not args.cleanup_tail or args.assigned
    assert not args.fixed_tail or args.phase=="offline"
    if args.phase=="offline":offline(args)
    else:live(args)

if __name__=="__main__":main()
