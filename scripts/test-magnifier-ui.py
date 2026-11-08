"""Finite real Ouroboros magnifier gate. Desktop source is limited to owned EDIT ROI."""
import argparse
import ctypes
from ctypes import wintypes
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import time
from PIL import Image

_spec=importlib.util.spec_from_file_location("magnifier_gate",Path(__file__).with_name("test-magnifier-acceptance.py"))
_module=importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)
user32,eventually,Rect=_module.user32,_module.eventually,_module.Rect
Client=_module._module._module._module._module.Client


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument("--executable",type=Path,required=True)
    parser.add_argument("--x86-executable",type=Path,required=True)
    parser.add_argument("--runtime",type=Path,required=True)
    parser.add_argument("--assigned-smoke",action="store_true")
    args=parser.parse_args()
    prefix="acceptance-assigned-magnifier-ui" if args.assigned_smoke else "acceptance-magnifier-ui"
    run=args.runtime.resolve()/time.strftime(prefix+"-%Y%m%d-%H%M%S")
    run.mkdir(parents=True)
    owned,client=[],None
    evidence={"result":"NOT ACCEPTED","samples":{},"scope":"Actual magnifier controls and decoded pixels; Desktop authorized only inside owned EDIT. Assigned variant is a bounded selected-root smoke."}
    try:
        images=[run/"control64.exe",run/"probe32.exe"]
        for source,image in zip((args.executable,args.x86_executable),images):
            shutil.copy2(source,image)
        for i,image in enumerate(images):
            color=_module.MagnifierGate.colors[i]
            owned.append(subprocess.Popen([str(image),"probe","--automation","--capture-pattern",str(color[0]<<16|color[1]<<8|color[2]),"--report",str(run/f"probe-{i}.json")],creationflags=subprocess.CREATE_NO_WINDOW))
        client=Client(images[0],args.runtime.resolve(),run/"discovery.stderr.log")
        assert {t["name"] for t in client.rpc("tools/list",{})["tools"]}==_module.MagnifierGate.tool_names
        pids=[p.pid for p in owned]
        inventory=eventually(lambda:client.tool("routing_windows"),lambda v:all(any(w["pid"]==p and w["title"].startswith("ProtoInputMultiPlex Automation Test") for w in v["windows"]) for p in pids),60)
        windows=[next(w for w in inventory["windows"] if w["pid"]==p and w["title"].startswith("ProtoInputMultiPlex Automation Test")) for p in pids]
        for i,window in enumerate(windows):
            assert user32.SetWindowPos(window["hwnd"],wintypes.HWND(-1),30+i*460,40,420,360,0x10)
        inventory=client.tool("routing_windows")
        windows=[next(w for w in inventory["windows"] if w["pid"]==p and w["title"].startswith("ProtoInputMultiPlex Automation Test")) for p in pids]
        points=[[w["client"]["left"]+120,w["client"]["top"]+100] for w in windows]
        if args.assigned_smoke:
            points=[[(w["client"]["left"]+w["client"]["right"])//2,(w["client"]["top"]+w["client"]["bottom"])//2] for w in windows]
        monitor=next(m for m in client.tool("routing_displays")["monitors"] if m["primary"] and m["stable_id"])
        client.close()
        client=None
        report=lambda i:json.loads((run/f"probe-{i}.json").read_text(encoding="utf-8"))
        eventually(lambda:report(0),lambda r:r["capture_hwnd"]!=0)
        rect=Rect()
        origin=wintypes.POINT()
        assert user32.GetClientRect(report(0)["capture_hwnd"],ctypes.byref(rect)) and user32.ClientToScreen(report(0)["capture_hwnd"],ctypes.byref(origin))
        region={"left":origin.x+60,"top":origin.y+40,"right":origin.x+188,"bottom":origin.y+136}
        assert origin.x<=region["left"]<region["right"]<=origin.x+rect.right and origin.y<=region["top"]<region["bottom"]<=origin.y+rect.bottom
        profile={"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":pids},
                 "seats":[{"name":f"Magnifier UI{i+1}","mouse":f"virtual://mouse/{i+1}","keyboard":f"virtual://keyboard/{i+1}","desktop_start":points[i],"cursor":{"lock_window":False}} for i in range(2)]}
        if args.assigned_smoke:
            profile["desktop"]=None
            for i,seat in enumerate(profile["seats"]):
                seat["target_pid"]=pids[i]
        (run/"profile.json").write_text(json.dumps(profile),encoding="utf-8")
        steps=[]
        def step(name,button,fields=None,wait=550):
            steps.append({"id":name,"button":button,"wait_ms":wait,"wait_for_sample":True,"fields":{k:str(v) for k,v in (fields or {}).items()}})
        step("defaults-page","Profiles")
        step("defaults-save","Save profile",{"profile_path":run/"defaults.json"})
        step("input-page","Input lab")
        step("start","Start routing",wait=1800)
        for i in range(2):
            step(f"select-{i}",f"{i+1} Hold click",wait=1200)
            step(f"drop-{i}",f"{i+1} Release click")
        step("hold-other","Hold key",{"selected_user":1,"key_name":"f5"})
        step("magnifier-page","Magnifier")
        step("lens-choice","Magnifier Lens")
        step("target-choice","Source selected target")
        lens={"magnifier_width":64,"magnifier_height":64,"magnifier_zoom":2,"magnifier_auto_refresh":"false","magnifier_border":"true","magnifier_border_red":7,"magnifier_border_green":29,"magnifier_border_blue":53,"magnifier_monitor_id":monitor["id"],"magnifier_work_area":"true"}
        step("lens-apply","Apply magnifier settings",lens)
        step("lens-refresh","Refresh magnifier")
        step("lens-save","Save magnified PNG",{"magnifier_export_path":run/"lens.png"})
        step("bad-save","Save magnified PNG",{"magnifier_export_path":run/"absent"/"bad.png"})
        step("docked-choice","Magnifier Docked")
        for side in ("left","right","top","bottom"):
            step("dock-"+side,"Dock "+side)
            step("dock-apply-"+side,"Apply magnifier settings")
        step("fullscreen-choice","Magnifier Fullscreen")
        step("fullscreen-apply","Apply magnifier settings",{"magnifier_zoom":32})
        step("fullscreen-refresh","Refresh magnifier")
        step("fullscreen-save","Save magnified PNG",{"magnifier_export_path":run/"fullscreen.png"})
        step("lens-return","Magnifier Lens")
        step("lens-return-apply","Apply magnifier settings",{"magnifier_zoom":2})
        step("desktop-choice","Source desktop pixels",{"magnifier_desktop_region":json.dumps(region)})
        step("desktop-apply","Apply magnifier settings")
        step("desktop-refresh","Refresh magnifier")
        step("desktop-save","Save magnified PNG",{"magnifier_export_path":run/"desktop.png"})
        step("desktop-new-refresh","Refresh magnifier")
        step("desktop-new-save","Save magnified PNG",{"magnifier_export_path":run/"desktop-new.png"})
        step("automatic-apply","Apply magnifier settings",{"magnifier_auto_refresh":"true","magnifier_refresh_hz":8})
        step("automatic-save","Save magnified PNG",{"magnifier_export_path":run/"automatic.png"},wait=1800)
        step("user-2","Select magnifier user 2")
        step("user-2-lens","Magnifier Lens")
        step("user-2-apply","Apply magnifier settings",{"magnifier_width":64,"magnifier_height":64,"magnifier_zoom":4,"magnifier_auto_refresh":"false","magnifier_border":"false"})
        step("user-2-refresh","Refresh magnifier")
        step("user-2-save","Save magnified PNG",{"magnifier_export_path":run/"second-user.png"})
        step("user-1","Select magnifier user 1")
        step("off-choice","Magnifier Off")
        step("off-apply","Apply magnifier settings")
        step("stop","Stop routing")
        step("save-page","Profiles")
        step("save-profile","Save profile",{"profile_path":run/"configured.json"})
        step("load-profile","Load profile",{"profile_path":run/"configured.json"})
        step("roundtrip-profile","Save profile",{"profile_path":run/"roundtrip.json"})
        if args.assigned_smoke:
            allowed={"input-page","start","hold-other","magnifier-page","lens-choice","target-choice","lens-apply","lens-refresh","lens-save","bad-save","docked-choice","dock-left","dock-apply-left","user-2","user-2-lens","user-2-apply","user-2-refresh","user-2-save","user-1","off-choice","off-apply","stop"}
            steps[:]=[s for s in steps if s["id"] in allowed]
        (run/"ui.json").write_text(json.dumps(steps),encoding="utf-8")
        owned.append(subprocess.Popen([str(images[0]),"gui","--profile",str(run/"profile.json"),"--ui-script",str(run/"ui.json")],cwd=Path(__file__).resolve().parent.parent,creationflags=subprocess.CREATE_NO_WINDOW))
        deadline,last_count,changed=time.monotonic()+300,0,time.monotonic()
        while time.monotonic()<deadline:
            assert owned[-1].poll() is None,"GUI exited before gate completion"
            try:
                events=[json.loads(line) for line in (run/"ui.events.jsonl").read_text(encoding="utf-8").splitlines()]
                if len(events)!=last_count:
                    last_count,changed=len(events),time.monotonic()
                assert time.monotonic()-changed<30,f"UI stalled after {last_count}/{len(steps)} controls"
                if not events or time.monotonic()-changed<.35 or not json.loads((run/"ui.ready.json").read_text(encoding="utf-8")):
                    time.sleep(.05)
                    continue
                name=steps[len(events)-1]["id"]
                if name in evidence["samples"]:
                    time.sleep(.05)
                    continue
                assert all(e["clicked"] for e in events)
                state=json.loads((run/"ui.state.json").read_text(encoding="utf-8")) if (run/"ui.state.json").exists() else []
                magnifier=json.loads((run/"ui.magnifier.json").read_text(encoding="utf-8")) if (run/"ui.magnifier.json").exists() else None
                acks=[json.loads(line) for line in (run/"ui.magnifier.acks.jsonl").read_text(encoding="utf-8").splitlines()] if (run/"ui.magnifier.acks.jsonl").exists() else []
                probes=[report(i) for i in range(2)]
                sample={"state":state,"magnifier":magnifier,"acks":acks,"probes":probes,"button":events[-1]["button"]}
            except (OSError,ValueError):
                time.sleep(.05)
                continue
            samples=evidence["samples"]
            if name.startswith("select-"):
                i=int(name[-1])
                if len(state)!=2 or state[i]["pid"]!=pids[i] or probes[i]["buttons"]!=1:
                    continue
            if name=="hold-other" and 116 not in probes[1]["held_keys"]:
                continue
            if name.endswith("apply") or name.startswith("dock-apply-"):
                user=1 if name=="user-2-apply" else 0
                expected="off" if name=="off-apply" else ("fullscreen" if name=="fullscreen-apply" else ("docked" if name.startswith("dock-apply-") else "lens"))
                if not magnifier or magnifier["users"][user]["settings"]["mode"]!=expected:
                    continue
                assert 116 in probes[1]["held_keys"],"magnifier settings released unrelated keyboard hold"
            if name.startswith("dock-apply-"):
                view=magnifier["users"][0]
                edge=name.removeprefix("dock-apply-")
                if view["settings"]["dock"]!=edge or not view["overlay"] or not view["overlay"]["visible"]:
                    continue
                assert view["overlay"]["bounds"][edge]==monitor["work_area"][edge]
            if name in ("lens-save","fullscreen-save","desktop-save","desktop-new-save","automatic-save","user-2-save"):
                filename={"lens-save":"lens.png","fullscreen-save":"fullscreen.png","desktop-save":"desktop.png","desktop-new-save":"desktop-new.png","automatic-save":"automatic.png","user-2-save":"second-user.png"}[name]
                if not (run/filename).exists():
                    continue
                with Image.open(run/filename) as image:
                    assert image.mode=="RGBA"
                    user=1 if name=="user-2-save" else 0
                    expected_color=tuple(_module.MagnifierGate.colors[user])
                    if name=="desktop-new-save":
                        expected_color=(37,149,211)
                    elif name=="automatic-save":
                        expected_color=(137,49,111)
                    assert image.getpixel((image.width//2,image.height//2))==(*expected_color,255),(name,image.getpixel((image.width//2,image.height//2)),expected_color)
                    if name=="fullscreen-save":
                        area=monitor["work_area"]
                        assert image.size==(area["right"]-area["left"],area["bottom"]-area["top"])
                    else:
                        assert image.size==(64,64)
                    if name=="lens-save":
                        assert image.getpixel((0,0))==(7,29,53,255)
                    sample["png"]={"path":str(run/filename),"size":list(image.size)}
                if name in ("lens-save","user-2-save"):
                    view=magnifier["users"][user]
                    assert view["source_pid"]==pids[user]
                    assert view["source_hwnd"]==(windows[user]["hwnd"] if args.assigned_smoke else probes[user]["capture_hwnd"])
                if name=="desktop-save":
                    assert magnifier["users"][0]["settings"]["desktop_region"]==region
                    sequence=magnifier["users"][0]["capture_sequence"]
                    color=(37,149,211)
                    assert user32.PostMessageW(probes[0]["capture_hwnd"],0x8051,color[0]<<16|color[1]<<8|color[2],0)
                    eventually(lambda:report(0),lambda p:p["capture_color"]==list(color))
                    time.sleep(.35)
                    stable=json.loads((run/"ui.magnifier.json").read_text(encoding="utf-8"))
                    assert stable["users"][0]["capture_sequence"]==sequence
                if name=="automatic-save":
                    assert magnifier["users"][0]["settings"]["auto_refresh"]
            if name=="automatic-apply":
                color=(137,49,111)
                assert user32.PostMessageW(probes[0]["capture_hwnd"],0x8051,color[0]<<16|color[1]<<8|color[2],0)
                eventually(lambda:report(0),lambda p:p["capture_color"]==list(color))
            if name=="bad-save":
                if not acks or not acks[-1]["error"]:
                    continue
                assert [u["settings"] for u in acks[-1]["users"]]==[u["settings"] for u in samples["lens-save"]["magnifier"]["users"]]
                assert magnifier["users"][0]["overlay"]["visible"] and 116 in probes[1]["held_keys"]
                assert not (run/"absent"/"bad.png").exists() and len(state)==2
            if name=="stop":
                handles=[u["overlay"]["hwnd"] for u in samples["user-2-save"]["state"]]
                handles += [u["overlay"]["hwnd"] for u in samples["user-2-save"]["magnifier"]["users"] if u["overlay"]]
                if any(user32.IsWindow(h) for h in handles) or any(p["held_keys"] or p["buttons"] for p in probes):
                    continue
            if name in ("defaults-save","save-profile","roundtrip-profile"):
                filename={"defaults-save":"defaults.json","save-profile":"configured.json","roundtrip-profile":"roundtrip.json"}[name]
                saved=json.loads((run/filename).read_text(encoding="utf-8"))
                sample["profile"]=saved
                if name=="defaults-save":
                    assert all(s["magnifier"]["mode"]=="off" and s["magnifier"]["source"]=="selected_target" and s["magnifier"]["zoom"]==2 for s in saved["seats"])
                else:
                    assert saved["seats"][0]["magnifier"]["mode"]=="off" and saved["seats"][0]["magnifier"]["desktop_region"]==region
                    assert saved["seats"][1]["magnifier"]["mode"]=="lens" and saved["seats"][1]["magnifier"]["zoom"]==4
                    if name=="roundtrip-profile":
                        assert saved==samples["save-profile"]["profile"]
            samples[name]=sample
            evidence["clicked_events"]=events
            (run/"evidence.json").write_text(json.dumps(evidence,indent=2),encoding="utf-8")
            (run/"ui.sampled.json").write_text(str(len(events)),encoding="utf-8")
            print(f"UI accepted {name}: {sample['button']}",flush=True)
            if name==("stop" if args.assigned_smoke else "roundtrip-profile"):
                assert len(samples)==len(steps)
                evidence["result"]="PASS"
                print(f"PASS: {len(steps)} actual magnifier controls. Evidence: {run}",flush=True)
                break
        else:
            raise AssertionError("Magnifier UI finite deadline exhausted")
    except Exception as error:
        evidence["failure"]=repr(error)
        print(f"NOT ACCEPTED: {error!a}. Evidence: {run}",flush=True)
        raise
    finally:
        if client:
            client.close()
        for process in reversed(owned):
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=15)
        evidence["owned_processes_closed"]=all(p.poll() is not None for p in owned)
        (run/"evidence.json").write_text(json.dumps(evidence,indent=2),encoding="utf-8")


if __name__=="__main__":
    main()
