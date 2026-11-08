"""Offline synthetic physical-path rejection; no DLLs, native workers or probes.

The GUI may perform its normal passive discovery. No discovered physical device
is selected, attached, or used. The only interface path is deliberately unresolved.
"""
import argparse
import copy
import importlib.util
from pathlib import Path
import time
from PIL import Image

spec=importlib.util.spec_from_file_location("physical_output_ui",Path(__file__).with_name("test-replication-graph-ui.py"))
ui=importlib.util.module_from_spec(spec);spec.loader.exec_module(ui)

def main():
    parser=argparse.ArgumentParser()
    for name in ("executable","x86-executable","runtime"):parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--ready",action="store_true")
    args=parser.parse_args();assert args.ready,"Wait for frozen producer build"
    gate=ui.UiGate(args,"output-physical-offline",offline=True,count=0)
    profile={"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":[]},
        "seats":[{"id":"synthetic-physical-output","name":"Owned unresolved physical boundary","mouse":"\\\\?\\HID#ProtoInputMultiPlexAcceptance_NotADevice","keyboard":None,"target_pid":0}]}
    invalid=copy.deepcopy(profile);invalid["seats"][0]["name"]="Rejected candidate name";invalid["seats"][0]["output"]={"buttons":[{"type":"legacy"}]*3+[{"type":"key","key":116}]+[{"type":"legacy"}],"keys":{}}
    ui.put(gate.run/"invalid-physical.json",invalid)
    steps=[ui.step("physical-page","Output Rules"),ui.step("physical-page-image","Output Rules",{"output_button":3,"output_kind":"key","output_key":"F5"}),
        ui.step("profiles","Profiles"),ui.step("invalid-load","Load profile",{"profile_path":gate.run/"invalid-physical.json"}),ui.step("save-retained","Save profile",{"profile_path":gate.run/"retained.json"})]
    image_wall=None
    def verify(name,sample):
        nonlocal image_wall
        assert not (gate.run/"ui.state.json").exists(),"Offline boundary unexpectedly started native routing"
        if name=="physical-page":
            image_wall=image_wall or time.time();png=gate.run/"ui.png"
            if not png.exists() or png.stat().st_mtime<image_wall:return False
            with Image.open(png) as image:image.load();image.save(gate.run/"physical-readonly.png")
            sample["readonly_screenshot_requires_independent_review"]=str(gate.run/"physical-readonly.png")
        if name=="save-retained":
            saved=ui.read_json(gate.run/"retained.json");seat=saved["seats"][0]
            assert seat["id"]==profile["seats"][0]["id"] and seat["name"]==profile["seats"][0]["name"]
            assert seat["mouse"]==profile["seats"][0]["mouse"] and seat["keyboard"] is None
            assert seat["output"]=={"buttons":[{"type":"legacy"}]*5,"keys":{}}
            sample["profile"]=saved
        return True
    try:gate.execute(profile,steps,verify)
    finally:
        for process in reversed(gate.owned):
            if process.poll() is None:process.terminate()
            process.wait(timeout=15)
        gate.evidence["owned_processes_closed"]=all(p.poll() is not None for p in gate.owned)
        gate.save()

if __name__=="__main__":main()
