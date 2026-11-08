"""Bounded independent profile document, real UI/file, and active-session gate.

Launch only after root READY. Native activity is limited to one owned virtual
target for disabled controls and read-only connected document operations.
"""
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
from PIL import Image

def module(name,file):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(file))
    result=importlib.util.module_from_spec(spec);spec.loader.exec_module(result);return result

ui=module("maintenance_ui","test-replication-graph-ui.py")
client_module=module("maintenance_client","test-mcp.py")
agents=module("maintenance_agents","test-connected-agents-acceptance.py")
MAX_BYTES=16*1024*1024
digest=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()

def fixtures():
    def profile(tag):
        return {"version":1,"concurrent":True,"desktop":None,
          "replication":{"version":1,"edges":[{"source":tag+"-a","target":tag+"-b","mouse":True,"keyboard":False,"text":True}]},
          "seats":[{"id":tag+suffix,"name":f"Owned {tag} {suffix} α","mouse":f"virtual://mouse/{i+1}","keyboard":f"virtual://keyboard/{i+1}","target_pid":987000+i,
            "cursor":{"shape":"ring" if i==0 else "crosshair","badge":tag,"bounds_mode":"region","region":{"left":-20,"top":30,"right":480,"bottom":360},"horizontal_wheel":"preserve","speed_x":1.25},
            "output":{"buttons":[{"type":"legacy"}]*3+[{"type":"key","key":116+i}]+[{"type":"disabled"}],"keys":{"118":{"type":"key","key":119+i}}},
            "annotation":{"tool":"off","width":9+i,"alpha":128,"color":[20,40,60]},
            "magnifier":{"mode":"off","zoom":3.0,"width":400,"height":200}}
            for i,suffix in enumerate(("-a","-b"))]}
    a,b=profile("A"),profile("B")
    b["replication"]["edges"][0].update(mouse=False,keyboard=True,text=False)
    b["seats"][0]["output"]["keys"]={"120":{"type":"disabled"}}
    b["seats"][0]["cursor"].update(shape="i_beam",badge="B")
    return a,b

def subset(expected,actual):
    if isinstance(expected,dict):
        for key,value in expected.items():assert key in actual;subset(value,actual[key])
    elif isinstance(expected,list):
        assert len(expected)==len(actual)
        for a,b in zip(expected,actual):subset(a,b)
    else:assert expected==actual,(expected,actual)

def doc(client,operation,value,error=False):
    text=value if isinstance(value,str) else json.dumps(value,ensure_ascii=False)
    return client.tool("profile_document",{"operation":operation,"document":text},error=error)

def document_gate(args):
    run=args.runtime.resolve()/time.strftime("acceptance-profile-documents-%Y%m%d-%H%M%S");run.mkdir()
    image=run/"control64.exe";shutil.copy2(args.executable,image)
    empty_runtime=run/"no-dlls";empty_runtime.mkdir()
    evidence={"result":"NOT ACCEPTED","build_sha256":digest(image),"checks":{}}
    client=None
    try:
        client=client_module.Client(image,empty_runtime,run/"mcp.stderr.log")
        tools=client.rpc("tools/list",{})["tools"]
        assert len(tools)==22 and {t["name"] for t in tools}==client_module.EXPECTED_TOOLS|{"profile_document"}
        assert all(t["inputSchema"]["type"]=="object" for t in tools)
        before=client.tool("routing_state");assert not before["running"]
        canonical=[]
        for value in fixtures():
            validated=doc(client,"validate",value);assert validated["valid"] and validated["seat_count"]==2
            assert validated["legacy_ids_materialized"] is False and "PID" in validated["stale_pid_reminder"]
            normalized=doc(client,"normalize",value);exported=doc(client,"export",normalized["json"])
            actual=json.loads(normalized["json"]);subset(value,actual)
            assert normalized["json"]==exported["json"] and not exported["legacy_ids_materialized"]
            canonical.append(actual)
        legacy={"version":1,"seats":[{"name":"Owned legacy","mouse":"virtual://mouse/41","keyboard":None,"target_pid":987100}]}
        migrated=doc(client,"normalize",legacy);again=doc(client,"normalize",migrated["json"])
        assert migrated["legacy_ids_materialized"] and not again["legacy_ids_materialized"]
        assert json.loads(migrated["json"])["seats"][0]["id"] and migrated["json"]==again["json"]
        invalid=copy.deepcopy(canonical[0]);invalid["replication"]["edges"].append({"source":"A-b","target":"A-a","mouse":True,"keyboard":True,"text":True})
        bad_output=copy.deepcopy(canonical[0]);bad_output["seats"][0]["output"]["keys"]={"118":{"type":"key","key":255}}
        for candidate in ("{",{"version":2,"seats":[]},invalid,bad_output):
            reply=doc(client,"normalize",candidate,error=True);assert reply["isError"]
        oversized=" "*(MAX_BYTES-1)+"☃"
        assert len(oversized)<MAX_BYTES+1 and len(oversized.encode("utf-8"))>MAX_BYTES
        reply=doc(client,"validate",oversized,error=True)
        assert "16 MiB" in " ".join(c.get("text","") for c in reply["content"])
        after=client.tool("routing_state");assert after==before
        assert not list(empty_runtime.iterdir())
        evidence["checks"]={"modern_full_profile_roundtrip":True,"legacy_ids_stable":True,"invalid_and_utf8_byte_limit_no_session_effect":True,"tools":len(tools)}
        for name,value in zip(("A","B"),canonical):ui.put(run/f"canonical-{name}.json",value)
        evidence["result"]="PASS"
        return canonical,run
    except Exception as error:evidence["failure"]=repr(error);raise
    finally:
        if client:client.close()
        evidence["owned_processes_closed"]=not client or client.process.poll() is not None
        ui.put(run/"evidence.json",evidence);print(f"{evidence['result']}: {run}",flush=True)

class ProfileGate(ui.UiGate):
    def __init__(self,args,label,offline=True,count=0):
        super().__init__(args,label,offline=offline,count=count)
        self.lock=None;self.connections=[]
    def close(self):
        self.unlock()
        for client in self.connections:client.close()
        for process in reversed(self.owned):
            if process.poll() is None:process.terminate()
            process.wait(timeout=15)
        self.evidence["owned_processes_closed"]=all(p.poll() is not None for p in self.owned);self.save()
    def snapshot(self):return ui.read_json(self.run/"ui.profile.json")
    def no_temps(self):assert not list(self.run.glob("*.tmp.*")),"Orphan profile temporary file"
    def unlock(self):
        if self.lock:
            kernel=ctypes.WinDLL("kernel32");kernel.CloseHandle.argtypes=[wintypes.HANDLE]
            assert kernel.CloseHandle(self.lock);self.lock=None
    def exclusive_read_lock(self,path):
        kernel=ctypes.WinDLL("kernel32",use_last_error=True)
        kernel.CreateFileW.argtypes=[wintypes.LPCWSTR,wintypes.DWORD,wintypes.DWORD,ctypes.c_void_p,wintypes.DWORD,wintypes.DWORD,wintypes.HANDLE]
        kernel.CreateFileW.restype=wintypes.HANDLE;kernel.CloseHandle.argtypes=[wintypes.HANDLE]
        self.lock=kernel.CreateFileW(str(path),0x80000000,1,None,3,0,None)
        assert self.lock not in (None,ctypes.c_void_p(-1).value),ctypes.get_last_error()
    def execute(self,profile,steps,verify):
        ui.put(self.run/"profile.json",profile);ui.put(self.run/"ui.json",steps)
        process=subprocess.Popen([str(self.images[0]),"gui","--profile",str(self.run/"profile.json"),"--ui-script",str(self.run/"ui.json")],
          cwd=self.run if self.offline else Path(__file__).resolve().parent.parent,creationflags=subprocess.CREATE_NO_WINDOW)
        self.owned.append(process)
        deadline=time.monotonic()+300;count=0;changed=time.monotonic()
        try:
            while time.monotonic()<deadline:
                assert process.poll() is None,"Own GUI exited prematurely"
                try:
                    events=[json.loads(line) for line in (self.run/"ui.events.jsonl").read_text(encoding="utf-8").splitlines()]
                    if len(events)!=count:count=len(events);changed=time.monotonic()
                    if not count and (before_first:=getattr(self,"before_first",None)):
                        before_first()
                    assert time.monotonic()-changed<35,f"No accepted control/sample after {count}/{len(steps)}"
                    if not count or time.monotonic()-changed<.35 or not ui.read_json(self.run/"ui.ready.json"):
                        time.sleep(.05);continue
                    target=steps[count-1];name=target["id"]
                    if name in self.evidence["samples"]:time.sleep(.05);continue
                    event=events[-1]
                    if target.get("expect_disabled"):
                        assert event["clicked"] is False and event.get("observed_disabled") is True,event
                        assert event.get("enabled") is False and event.get("visible") is True,event
                    else:assert event["clicked"] is True and event["button"]==target["button"],event
                    sample={"progress":ui.read_json(self.run/"ui.progress.json"),"probes":self.probes(),"state":ui.read_json(self.run/"ui.state.json") if (self.run/"ui.state.json").exists() else [],"event":event}
                    if not verify(name,sample):time.sleep(.05);continue
                    self.evidence["samples"][name]=sample;self.evidence["clicked_events"]=events
                    self.save();(self.run/"ui.sampled.json").write_text(str(count),encoding="utf-8")
                    print(f"UI accepted {name}: {target['button']} ({'disabled observation' if target.get('expect_disabled') else 'click'})",flush=True)
                    if count==len(steps):
                        self.evidence["result"]="PASS";print(f"PASS: {len(steps)} accepted steps. Evidence: {self.run}",flush=True);return
                except (OSError,ValueError):time.sleep(.05)
            raise AssertionError("Finite profile UI deadline exhausted")
        except Exception as error:
            self.evidence["failure"]=repr(error);print(f"NOT ACCEPTED: {error!a}. Evidence: {self.run}",flush=True);raise
        finally:self.save()

def offline_gate(args,canonical):
    gate=ProfileGate(args,"profile-maintenance-offline-tail" if args.tail else "profile-maintenance-offline")
    a,b=canonical;main=gate.run/"active.json";backup=Path(str(main)+".bak")
    a_original=(" \r\n"+json.dumps(a,ensure_ascii=False,indent=1)+"\r\n").encode("utf-8")
    main.write_bytes(a_original);backup.write_bytes(json.dumps(b).encode())
    ui.put(gate.run/"import-B.json",b);(gate.run/"invalid.json").write_text("{",encoding="utf-8")
    if not args.tail:(gate.run/"oversized.json").write_bytes((" "*(MAX_BYTES-1)+"☃").encode("utf-8"))
    step=ui.step
    steps=[step("page","Profiles",{"profile_path":main}),step("page-image","Profiles"),step("save-a","Save profile"),
      step("stage-output","Output Rules",{"output_user":1,"output_button":3,"output_kind":"key","output_key":"f9"}),
      step("stage-graph","Replication Graph"),step("draft-clear","Clear replication graph"),step("profiles-import","Profiles"),
      step("import-b","Import profile",{"profile_transfer_path":gate.run/"import-B.json"}),
      step("reset-output-page","Output Rules"),step("reset-output-save","Save output rules to profile"),
      step("reset-graph-page","Replication Graph"),step("save-reset-graph","Save graph JSON",{"graph_file_path":gate.run/"reset-graph.json"}),
      step("save-page","Profiles"),step("save-b","Save profile"),step("export-b","Export profile",{"profile_transfer_path":gate.run/"export-B.json"}),
      step("invalid-import","Import profile",{"profile_transfer_path":gate.run/"invalid.json"}),
      step("oversized-import","Import profile",{"profile_transfer_path":gate.run/"oversized.json"}),
      step("restore-a","Restore profile backup"),step("reset-empty","Reset profile"),step("undo-reset","Undo profile change"),
      step("undo-once-only","Undo profile change",disabled=True),step("bad-backup","Restore profile backup"),
      step("bad-main-save","Save profile"),step("bad-main-restore","Restore profile backup"),
      step("import-lock-candidate","Import profile",{"profile_transfer_path":gate.run/"import-B.json"}),step("locked-save","Save profile"),
      step("save-after-unlock","Save profile"),step("final-export","Export profile",{"profile_transfer_path":gate.run/"final-export.json"}),
      step("final-load","Load profile",{"profile_path":gate.run/"final-export.json"})]
    expected=a;stored_a=None;stored_b=None;image_wall=None
    if args.tail:
        indexed={entry["id"]:entry for entry in steps}
        steps=[indexed[name] for name in ("page","reset-empty","undo-reset","undo-once-only","bad-backup","bad-main-save","bad-main-restore","import-lock-candidate","locked-save","save-after-unlock","final-export","final-load")]
        stored_a=a_original;stored_b=backup.read_bytes()
    def verify(name,sample):
        nonlocal expected,stored_a,stored_b,image_wall
        snapshot=gate.snapshot();sample["profile_state"]=snapshot
        if name=="page" and not args.tail:
            image_wall=image_wall or time.time();png=gate.run/"ui.png"
            if not png.exists() or png.stat().st_mtime<image_wall:return False
            with Image.open(png) as image:image.load();image.save(gate.run/"profile-controls.png")
        if name=="save-a":
            assert backup.read_bytes()==a_original and json.loads(main.read_bytes())==a
            stored_a=main.read_bytes();gate.no_temps()
        if name=="import-b":expected=b;assert main.read_bytes()==stored_a
        if name=="save-reset-graph":assert ui.read_json(gate.run/"reset-graph.json")==b["replication"]
        if name=="save-b":
            assert backup.read_bytes()==stored_a and json.loads(main.read_bytes())==b
            stored_b=main.read_bytes();gate.no_temps()
        if name=="export-b":assert ui.read_json(gate.run/"export-B.json")==b and main.read_bytes()==stored_b and backup.read_bytes()==stored_a
        if name in ("invalid-import","oversized-import"):
            assert main.read_bytes()==stored_b and backup.read_bytes()==stored_a
            assert "parse" in snapshot["status"].lower() if name=="invalid-import" else "16 MiB" in snapshot["status"]
        if name=="restore-a":
            expected=a;assert json.loads(main.read_bytes())==a and backup.read_bytes()==stored_b;gate.no_temps()
        if name=="reset-empty":
            expected={"version":1,"concurrent":False,"desktop":None,"replication":{"version":1,"edges":[]},"seats":[]}
            assert main.read_bytes()==stored_a and backup.read_bytes()==stored_b and snapshot["undo_available"]
        if name=="undo-reset":expected=a;assert not snapshot["undo_available"]
        if name=="undo-once-only":backup.write_bytes(b"{bad backup");sample["prepared_invalid_backup_sha256"]=digest(backup)
        if name=="bad-backup":
            assert main.read_bytes()==stored_a and backup.read_bytes()==b"{bad backup"
            assert "backup" in snapshot["status"].lower();backup.write_bytes(stored_b);main.write_bytes(b"{bad main")
        if name in ("bad-main-save","bad-main-restore"):
            assert main.read_bytes()==b"{bad main" and backup.read_bytes()==stored_b
            assert "invalid existing" in snapshot["status"].lower();gate.no_temps()
            if name=="bad-main-restore":main.write_bytes(stored_a)
        if name=="import-lock-candidate":expected=b;gate.exclusive_read_lock(main)
        if name=="locked-save":
            assert main.read_bytes()==stored_a and backup.read_bytes()==stored_a
            assert "atomically replace" in snapshot["status"].lower();gate.no_temps()
            gate.unlock()
        if name=="save-after-unlock":assert ui.read_json(main)==b and backup.read_bytes()==stored_a;gate.no_temps()
        if name in ("final-export","final-load"):assert ui.read_json(gate.run/"final-export.json")==b
        assert snapshot["profile"]==expected,(name,"Full draft changed unexpectedly")
        return True
    try:gate.execute(a,steps,verify);return gate.run
    finally:gate.close()

def active_gate(args,canonical):
    gate=ProfileGate(args,"profile-maintenance-active",offline=False,count=1)
    process=subprocess.Popen([str(gate.images[0]),"probe","--automation","--report",str(gate.run/"probe-0.json")],creationflags=subprocess.CREATE_NO_WINDOW)
    gate.owned.append(process);gate.pids=[process.pid]
    ui.eventually(gate.probes,lambda reports:reports[0]["frames"]>2)
    identity="profile-maintenance-own-virtual"
    profile={"version":1,"concurrent":True,"desktop":None,"seats":[{"id":identity,"name":"Owned profile active target","mouse":"virtual://mouse/61","keyboard":"virtual://keyboard/61","target_pid":process.pid}]}
    main=gate.run/"profile.json";transfer=gate.run/"forbidden-export.json"
    steps=[ui.step("profiles-before-start","Profiles"),ui.step("load-for-undo","Load profile",{"profile_path":main}),
      ui.step("lab","Input lab"),ui.step("start","Start routing"),ui.step("focus","1 Hold click"),ui.step("focus-up","1 Release click"),
      ui.step("hold","Hold key",{"selected_user":0,"key_name":"f5"}),ui.step("active-profiles","Profiles",{"profile_transfer_path":transfer}),
      ui.step("disabled-save","Save profile",disabled=True),ui.step("disabled-load","Load profile",disabled=True),
      ui.step("disabled-import","Import profile",disabled=True),ui.step("disabled-export","Export profile",disabled=True),
      ui.step("disabled-restore","Restore profile backup",disabled=True),ui.step("disabled-reset","Reset profile",disabled=True),
      ui.step("disabled-undo","Undo profile change",disabled=True),ui.step("agent-page","Agent Access"),
      ui.step("connected-documents","Enable agent access",{"agent_users":identity,"agent_pids":process.pid,"agent_capture":"false","agent_session_control":"false"}),
      ui.step("stop","Stop routing")]
    initial_profile=None;initial_bytes=None;overlays=[];connected_done=False;descriptor=None
    def verify(name,sample):
        nonlocal initial_profile,initial_bytes,overlays,connected_done,descriptor
        snapshot=gate.snapshot();sample["profile_state"]=snapshot
        if name=="load-for-undo":
            assert snapshot["undo_available"];initial_profile=snapshot["profile"];initial_bytes=main.read_bytes()
        if name=="start":
            if len(sample["state"])!=1:return False
            overlays=[sample["state"][0]["overlay"]["hwnd"]]
        if name=="focus" and not sample["probes"][0]["buttons"]&1:return False
        if name=="focus-up" and sample["probes"][0]["buttons"]:return False
        if name=="hold" and 116 not in sample["probes"][0]["held_keys"]:return False
        if name.startswith("disabled-"):
            assert snapshot["profile"]==initial_profile and main.read_bytes()==initial_bytes
            assert 116 in sample["probes"][0]["held_keys"] and not transfer.exists()
        if name=="connected-documents" and not connected_done:
            paths=list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{gate.owned[-1].pid}-*.json"))
            assert len(paths)==1;descriptor=paths[0]
            client=agents.ConnectedMcp(gate.images[0],descriptor,gate.run/"connected.stderr.log");gate.connections.append(client)
            tools=client.rpc("tools/list",{})["tools"]
            assert len(tools)==22 and {t["name"] for t in tools}==client_module.EXPECTED_TOOLS|{"profile_document"}
            before=gate.probes()[0];state_before=client.tool("routing_state")
            for operation in ("validate","normalize","export"):
                result=doc(client,operation,canonical[1]);assert result["valid"]
                if operation!="validate":assert json.loads(result["json"])==canonical[1]
            doc(client,"normalize",{"version":2,"seats":[]},error=True)
            state_after=client.tool("routing_state");after=gate.probes()[0]
            assert state_after["running"] and not state_after["paused"]
            assert state_after["users"]==state_before["users"]
            assert state_after["user_settings"]==state_before["user_settings"] and state_after["output"]==state_before["output"]
            assert before["key_events"]==after["key_events"] and before["mouse_events"]==after["mouse_events"] and 116 in after["held_keys"]
            assert gate.snapshot()["profile"]==initial_profile and main.read_bytes()==initial_bytes and not transfer.exists()
            client.close();gate.connections.remove(client);connected_done=True
            sample["connected_document_check"]={"valid_operations":3,"invalid_rejected":True,"native_hold_and_draft_unchanged":True,"no_file_export":True}
        if name=="stop":
            probe=sample["probes"][0]
            if len(probe["key_events"])<2:return False
            assert [(event["vk"],event["pressed"]) for event in probe["key_events"]]==[(116,True),(116,False)]
            if probe["held_keys"] or probe["buttons"] or any(ui.user32.IsWindow(hwnd) for hwnd in overlays):return False
            assert not descriptor or not descriptor.exists()
        return True
    try:gate.execute(profile,steps,verify);return gate.run
    finally:
        gate.close()
        if descriptor and descriptor.exists():
            owner=json.loads(descriptor.read_text())["controller_pid"]
            assert any(p.pid==owner and p.poll() is not None for p in gate.owned)
            descriptor.unlink();gate.evidence["runner_removed_owned_stale_descriptor"]=True;gate.save()

def main():
    parser=argparse.ArgumentParser()
    for name in ("executable","x86-executable","runtime"):parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--ready",action="store_true");parser.add_argument("--phase",choices=("documents","offline","active"),default="documents")
    parser.add_argument("--canonical-dir",type=Path)
    parser.add_argument("--tail",action="store_true",help="Focused Undo/failure/replacement tail after accepted offline prefix")
    args=parser.parse_args();assert args.ready,"No launch before root READY/exclusive slot"
    if args.phase=="documents":document_gate(args)
    else:
        assert args.canonical_dir
        canonical=[ui.read_json(args.canonical_dir/f"canonical-{name}.json") for name in ("A","B")]
        if args.phase=="offline":offline_gate(args,canonical)
        else:active_gate(args,canonical)

if __name__=="__main__":main()
