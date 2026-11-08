"""Finite embedded Draw/Magnifier/Macros binding gate; launch only after READY."""
import argparse
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import time


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def profile_fixture():
    return {"version":1,"concurrent":True,"desktop":None,"seats":[
        {"id":f"tools-owned-{i}","name":"Owned Alpha" if i==0 else "Owned Beta",
         "mouse":f"virtual://mouse/{i+1}","keyboard":f"virtual://keyboard/{i+1}","target_pid":987101+i,
         "cursor":{"color":[70,190,160] if i==0 else [235,155,65]},
         "annotation":{"tool":"off","width":7+6*i,"color":[240,180,70] if i==0 else [30,140,210],"alpha":155},
         "magnifier":{"mode":"off","source":"selected_target","zoom":1.5+i,"width":180+40*i,
             "height":100+20*i,"auto_refresh":False}}
        for i in range(2)]}


def workflow(args):
    lifecycle=module("embedded_tools_lifecycle","test-app-lifecycle-ui.py")
    Base,ui,hotkeys,geometry=lifecycle.gate_type()
    class Gate(Base):
        def __init__(self):
            super().__init__(args,"per-user-tools-"+args.phase,offline=args.phase!="live",count=2 if args.phase=="live" else 0)
            self.frames={};self.started_frames={};self.requested={}
        def frame(self,name,size):
            if name in self.frames:return self.frames[name]
            hwnd=self.main_hwnd()
            if not hwnd:return None
            if name not in self.requested:
                geometry.resize_owned_client(hwnd,size);self.requested[name]=size
                self.started_frames[name]=time.time();return None
            current=geometry.geometry(hwnd)
            if any(abs(a-b)>2 for a,b in zip(current["client_logical"],size)):return None
            shot=geometry.copy_owned_product_frame(self.run/"ui.png",self.run/(name+".png"),hwnd,self.started_frames[name])
            if shot:shot["geometry"]=current;self.frames[name]=shot
            return shot
        def pair(self,name):
            for suffix,size in (("normal",[540,520]),("minimum",[320,360])):
                if not self.frame(name+"-"+suffix,size):return False
            return True
        def acks(self,name):
            p=self.run/("ui."+name+".acks.jsonl")
            return [json.loads(line) for line in p.read_text(encoding="utf-8").splitlines()] if p.exists() else []
    g=Gate();profile=profile_fixture();baseline=None;expected=None;transitions={};layers=set();ack_counts={}
    prefs={"version":1,"appearance_mode":"light" if args.phase=="live" else "dark","zoom_percent":100,
        "launch_minimized":False,"minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,
        "shortcuts":[]}
    step=ui.step
    generator={"studio_pattern":"jiggle","studio_points":8,"studio_duration_ms":300,
        "studio_radius":9,"studio_seed":42,"studio_click":"none"}
    if args.phase!="live":
        steps=[step("select-b","Select user 2"),step("configure-b","Configure user"),step("draw-b","Draw"),
            step("brush-b","Annotation Brush"),step("save-draw-b","Save annotation settings to profile",{"annotation_width":31}),
            step("mag-b","Magnifier"),step("lens-b","Magnifier Lens"),
            step("save-mag-b","Save magnifier settings to profile",{"magnifier_zoom":3.5}),
            step("macros-b","Macros"),step("macro-offline-disabled","Generate user macro",generator,disabled=True),
            step("home-b","Back to Home"),step("select-a","Select user 1"),step("configure-a","Configure user"),
            step("draw-a","Draw"),step("mag-a","Magnifier"),step("macros-a","Macros"),step("home-a","Back to Home"),
            step("settings-draw","Settings"),step("global-draw","Annotation"),step("back-draw","Back to Home"),
            step("settings-mag","Settings"),step("global-mag","Magnifier"),step("back-mag","Back to Home"),
            step("settings-macro","Settings"),step("global-macro","Macro Studio"),step("back-macro","Back to Home")]
        if args.phase=="visual":
            assert args.from_run
            prior=ui.read_json(args.from_run/"evidence.json")
            assert prior["control_and_profile_result"]=="PASS" and prior["owned_processes_closed"]
            assert len(prior["samples"])==26 and prior["result"]=="PENDING INDEPENDENT VISUAL REVIEW"
            assert set(prior["build_sha256"].values())=={
                "55d755afc5666bc19fd9de53a5dea05e8bf6e41eb419a0b02c80416e289b90ee",
                "6d80848455b515bdb0eb8ffdd70e210cb990b031fc2bbe7b51d12a192fde6055"}
            profile=copy.deepcopy(ui.read_json(args.from_run/"final-profile.json"))
            assert profile["seats"][1]["annotation"]["width"]==31
            assert profile["seats"][1]["magnifier"]["zoom"]==3.5
            g.evidence["retained_offline"]={"path":str(args.from_run),"sha256":hashlib.sha256((args.from_run/"evidence.json").read_bytes()).hexdigest(),
                "scope":"26 accepted control/profile observations retained; only corrected tab/generator pixels, no save/global flow replay"}
            steps=[step("select-b","Select user 2"),step("configure-b","Configure user"),step("input-b","Input"),step("macros-b","Macros"),
                step("generator-upper","Retime selected event",disabled=True),
                step("generator-lower","Generate user macro",generator,disabled=True),
                step("draw-b","Draw"),step("mag-b","Magnifier"),step("lock-b","Lock")]
    else:
        assert args.from_run
        prior=ui.read_json(args.from_run/"evidence.json")
        assert prior["control_and_profile_result"]=="PASS" and prior["owned_processes_closed"]
        profile=copy.deepcopy(ui.read_json(args.from_run/"final-profile.json"))
        g.evidence["prior_offline"]={"path":str(args.from_run),"sha256":hashlib.sha256((args.from_run/"evidence.json").read_bytes()).hexdigest()}
        for i in range(2):
            p=subprocess.Popen([str(g.images[0]),"probe","--automation","--capture-pattern",str(0x335577+0x442200*i),
                "--report",str(g.run/f"probe-{i}.json")],creationflags=subprocess.CREATE_NO_WINDOW)
            g.owned.append(p);g.pids.append(p.pid);profile["seats"][i]["target_pid"]=p.pid
        steps=[step("start","Start routing"),step("select-b","Select user 2"),step("configure-b","Configure user"),
            step("draw-b","Draw"),step("apply-draw-b","Apply annotation settings",{"annotation_width":33}),
            step("mag-b","Magnifier"),step("apply-mag-b","Apply magnifier settings",{"magnifier_zoom":4.5}),
            step("macros-b","Macros"),step("generate-b","Generate user macro",generator),
            step("cancel-disabled","Cancel user playback",disabled=True),step("home","Back to Home"),step("stop","Stop routing")]
    # Recipients must come only from real Home selection and tab focus.
    forbidden={"selected_user","annotation_user","magnifier_user","studio_user"}
    assert not any(forbidden.intersection(s.get("fields",{})) for s in steps)

    def transition(name,current,field,changes):
        nonlocal expected
        if name not in transitions:
            after=copy.deepcopy(expected);after["seats"][1][field].update(changes)
            transitions[name]=(copy.deepcopy(expected),after)
        before,after=transitions[name]
        if current==before and current!=after:return False
        assert current==after,"Unrelated profile/user mutation"
        expected=copy.deepcopy(after);return True

    def verify(name,sample):
        nonlocal baseline,expected
        current=g.snapshot()["profile"]
        if baseline is None:
            if args.phase!="offline":assert current==profile,"Continuation/start changed accepted seed profile"
            baseline=copy.deepcopy(current);expected=copy.deepcopy(current)
        edits={"save-draw-b":("annotation",{"tool":"brush","width":31}),
            "save-mag-b":("magnifier",{"mode":"lens","zoom":3.5}),
            "apply-draw-b":("annotation",{"width":33}),"apply-mag-b":("magnifier",{"zoom":4.5})}
        if name in edits:
            if not transition(name,current,*edits[name]):return False
        else:assert current==expected,"Tab/user/global navigation changed complete profile"
        sample["complete_profile_verified"]=True
        if args.phase!="live":
            if name in {"brush-b","lens-b","macro-offline-disabled"}:
                if not g.pair({"brush-b":"draw-dark","lens-b":"magnifier-dark","macro-offline-disabled":"macros-dark"}[name]):return False
            if name in {"draw-a","mag-a","macros-a","global-draw","global-mag","global-macro"}:
                if not g.frame(name,[540,520]):return False
            if args.phase=="visual" and name in {"generator-upper","generator-lower"}:
                if not g.pair("corrected-"+name):return False
            if args.phase=="visual" and name in {"input-b","macros-b","draw-b","mag-b","lock-b"}:
                if not g.frame("corrected-"+name+"-minimum",[320,360]):return False
            assert not (g.run/"ui.state.json").exists(),"Stopped offline phase created worker state"
        else:
            if name=="start":
                if len(sample["state"])!=2:return False
                layers.update(s["overlay"]["hwnd"] for s in sample["state"] if s.get("overlay"))
                ack_counts.update({k:len(g.acks(k)) for k in ("annotation","magnifier","macro-studio")})
            if name in {"apply-draw-b","apply-mag-b","generate-b"}:
                channel={"apply-draw-b":"annotation","apply-mag-b":"magnifier","generate-b":"macro-studio"}[name]
                replies=g.acks(channel)
                if len(replies)<ack_counts[channel]+1:return False
                assert len(replies)==ack_counts[channel]+1,"Unexpected extra feature ACK"
                reply=replies[-1];assert reply["acknowledged"] and not reply.get("error")
                sample["worker_ack"]=reply
                if name!="generate-b":
                    field="annotation" if channel=="annotation" else "magnifier"
                    assert reply["users"][0]["settings"]==expected["seats"][0][field]
                    assert reply["users"][1]["settings"]==expected["seats"][1][field]
                else:
                    assert reply["users"][0]["loaded_events"]==0 and reply["users"][1]["loaded_events"]>0
                    clip=reply["users"][1].get("clip");assert clip and all(event["action"]["user"]==1 for event in clip["events"])
                    assert not reply["users"][1].get("playback"),"Generate unexpectedly played"
                if name=="apply-mag-b":
                    mag=ui.read_json(g.run/"ui.magnifier.json");b=mag["users"][1]
                    assert all(u["settings"]["source"]=="selected_target" for u in mag["users"])
                    if b.get("source_pid")!=g.pids[1] or not b["capture_sequence"] or b["in_flight"]:return False
                    if b.get("overlay"):layers.add(b["overlay"]["hwnd"])
                    sample["owned_selected_target_frame"]=b
                if name=="apply-draw-b" and reply.get("overlay"):layers.add(reply["overlay"]["hwnd"])
                if not g.pair({"apply-draw-b":"draw-light","apply-mag-b":"magnifier-light","generate-b":"macros-light"}[name]):return False
            if name=="cancel-disabled" and not g.frame("macros-light-disabled",[320,360]):return False
            if name=="stop" and any(ui.user32.IsWindow(h) for h in layers):return False
        return True

    def before(gate):return gate.main_hwnd() is not None
    def after(gate):
        ui.put(gate.run/"final-profile.json",gate.snapshot()["profile"])
        gate.expected_exit=True
        assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0)
        gate.gui.wait(timeout=10);assert gate.gui.returncode==0 and not hotkeys.lifecycle_window(gate.gui.pid)
        return True
    try:
        provenance=g.run/"runner-source";provenance.mkdir();shutil.copy2(Path(__file__),provenance/Path(__file__).name)
        g.evidence["scope"]="Embedded tool recipient/save/ACK/layout integration only; i686 copied unused; selected_target owned probes only; no algorithm/input/playback or pending-retarget-race claim"
        g.execute(profile,prefs,steps,verify,before,after)
        g.evidence.update(captures=g.frames,control_and_profile_result="PASS",result="PENDING INDEPENDENT VISUAL REVIEW")
        g.save();return g.run
    finally:
        g.close();lifecycle.remove_startup_key(g.startup_key)


def main():
    parser=argparse.ArgumentParser();parser.add_argument("--ready",action="store_true")
    parser.add_argument("--phase",choices=("offline","visual","live"),required=True);parser.add_argument("--from-run",type=Path)
    for name in ("executable","x86-executable","runtime"):parser.add_argument("--"+name,type=Path,required=True)
    args=parser.parse_args();assert args.ready,"Await frozen images and exclusive slot"
    workflow(args)


if __name__=="__main__":main()
