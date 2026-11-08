"""Ten-control offline annotation profile cache boundary; no routing/probes/DLLs."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import time


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument("--executable",type=Path,required=True)
    parser.add_argument("--runtime",type=Path,required=True)
    args=parser.parse_args()
    run=args.runtime.resolve()/time.strftime("acceptance-annotation-profile-ui-%Y%m%d-%H%M%S")
    run.mkdir(parents=True)
    image=run/"offline-gui.exe"
    shutil.copy2(args.executable,image)
    defaults={"tool":"off","activator":"primary","color":[96,165,250],"alpha":255,"width":6,"clear_key":None,"clear_all":False}
    def profile(prefix,base):
        return {"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":[]},
                "seats":[{"name":f"{prefix} user {i+1}","mouse":f"virtual://mouse/{base+i}","keyboard":f"virtual://keyboard/{base+i}","target_pid":0,"annotation":dict(defaults)} for i in range(2)]}
    a,b=profile("Profile A",61),profile("Profile B",71)
    for name,value in (("a",a),("b",b)):
        (run/f"{name}.json").write_text(json.dumps(value),encoding="utf-8")
    steps=[]
    def step(name,button,fields=None):
        steps.append({"id":name,"button":button,"wait_ms":500,"wait_for_sample":True,"fields":{k:str(v) for k,v in (fields or {}).items()}})
    step("annotation-a","Annotation")
    step("stage-user-2","Select annotation user 2")
    step("stage-unapplied","Annotation Brush",{"annotation_width":43,"annotation_red":17,"annotation_green":73,"annotation_blue":129,"annotation_clear_key":"F8"})
    step("profiles-load","Profiles")
    step("load-b","Load profile",{"profile_path":run/"b.json"})
    step("annotation-b","Annotation")
    step("fresh-brush","Annotation Brush")
    step("commit-b","Save annotation settings to profile")
    step("profiles-save","Profiles")
    step("save-b","Save profile",{"profile_path":run/"roundtrip-b.json"})
    (run/"ui.json").write_text(json.dumps(steps),encoding="utf-8")
    evidence={"result":"NOT ACCEPTED","scope":"Offline profile transition resets staged annotation selection, width/color/clear-key drafts before committing the newly loaded profile.","sha256_x64":hashlib.sha256(image.read_bytes()).hexdigest(),"samples":{},"profile_a":a,"profile_b":b}
    process=None
    try:
        process=subprocess.Popen([str(image),"gui","--profile",str(run/"a.json"),"--ui-script",str(run/"ui.json")],cwd=Path(__file__).resolve().parent.parent,creationflags=subprocess.CREATE_NO_WINDOW)
        deadline,last_count,changed=time.monotonic()+100,0,time.monotonic()
        while time.monotonic()<deadline:
            assert process.poll() is None,"GUI exited early"
            try:
                events=[json.loads(line) for line in (run/"ui.events.jsonl").read_text(encoding="utf-8").splitlines()]
                if len(events)!=last_count:
                    last_count,changed=len(events),time.monotonic()
                assert time.monotonic()-changed<25,f"Offline UI did not settle after {last_count}/10 controls"
                if not events or not json.loads((run/"ui.ready.json").read_text(encoding="utf-8")):
                    time.sleep(.05)
                    continue
                name=steps[len(events)-1]["id"]
                if name in evidence["samples"]:
                    time.sleep(.05)
                    continue
                assert all(e["clicked"] for e in events)
                sample={"button":events[-1]["button"]}
                if name=="stage-unapplied" and (run/"ui.png").exists():
                    shutil.copy2(run/"ui.png",run/"staged-a.png")
                    sample["screenshot"]=str(run/"staged-a.png")
                if name=="save-b":
                    saved=json.loads((run/"roundtrip-b.json").read_text(encoding="utf-8"))
                    sample["saved_profile"]=saved
                    assert [s["name"] for s in saved["seats"]]==[s["name"] for s in b["seats"]]
                    for i,seat in enumerate(saved["seats"]):
                        assert seat["mouse"]==b["seats"][i]["mouse"] and seat["keyboard"]==b["seats"][i]["keyboard"]
                        expected={**defaults,"tool":"brush" if i==0 else "off"}
                        assert seat["annotation"]==expected,(i,expected,seat["annotation"])
                    assert not (run/"ui.annotation.json").exists(),"Offline boundary unexpectedly started annotation worker"
                evidence["samples"][name]=sample
                evidence["clicked_events"]=events
                (run/"evidence.json").write_text(json.dumps(evidence,indent=2),encoding="utf-8")
                (run/"ui.sampled.json").write_text(str(len(events)),encoding="utf-8")
                print(f"UI accepted {name}: {sample['button']}",flush=True)
                if name=="save-b":
                    assert len(evidence["samples"])==10
                    evidence["result"]="PASS"
                    print(f"PASS: 10 offline annotation profile controls. Evidence: {run}",flush=True)
                    break
            except (OSError,ValueError):
                time.sleep(.05)
        else:
            raise AssertionError("Offline profile UI finite deadline exhausted")
    except Exception as error:
        evidence["failure"]=repr(error)
        print(f"NOT ACCEPTED: {error!a}. Evidence: {run}",flush=True)
        raise
    finally:
        if process and process.poll() is None:
            process.terminate()
            process.wait(timeout=15)
        evidence["owned_processes_closed"]=bool(process and process.poll() is not None)
        (run/"evidence.json").write_text(json.dumps(evidence,indent=2),encoding="utf-8")


if __name__=="__main__":
    main()
