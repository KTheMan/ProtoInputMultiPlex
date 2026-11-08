"""Focused actual Output Rules UI, persistence, and connected permission gate.

No live launch before READY. Descriptor credentials stay in memory; evidence
contains only acknowledged non-secret state and own native fixture messages.
"""
import argparse
from ctypes import wintypes
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import time
from PIL import Image

def module(name,file):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(file))
    result=importlib.util.module_from_spec(spec); spec.loader.exec_module(result); return result

_ui=module("output_ui_helpers","test-replication-graph-ui.py")
_native=module("output_native_helpers","test-output-rules-acceptance.py")
_agents=module("output_agent_helpers","test-connected-agents-acceptance.py")
step,read_json,eventually=_ui.step,_ui.read_json,_ui.eventually


class OutputUi(_ui.UiGate):
    def __init__(self,args):
        super().__init__(args,"output-rules-stop" if args.stop_only else "output-rules-connected-tail" if args.connected_tail else "output-rules-connected" if args.connected else "output-rules",count=3 if args.connected or args.connected_tail else 2)
        self.clients=[]; self.connections=[]; self.descriptor=None; self.once={}
        self.queue_baseline=None
        self.ids=["output-own-a","output-own-b","output-own-no-keyboard"]

    def fixtures(self):
        for i in range(self.count):
            self.owned.append(subprocess.Popen([str(self.images[i%2]),"probe","--automation","--report",str(self.run/f"probe-{i}.json")],creationflags=subprocess.CREATE_NO_WINDOW))
        self.pids=[p.pid for p in self.owned]
        client=_ui.Client(self.images[0],self.args.runtime.resolve(),self.run/"discovery.stderr.log")
        try:
            tools=client.rpc("tools/list",{})["tools"]
            assert {t["name"] for t in tools}==_native.OutputGate.tool_names
            assert all(t["inputSchema"]["type"]=="object" for t in tools)
            self.evidence["tools"]=tools
            inventory=eventually(lambda:client.tool("routing_windows"),lambda v:all(any(w["pid"]==p and w["title"].startswith(_native._base.PREFIX) for w in v["windows"]) for p in self.pids),60)
            for i,pid in enumerate(self.pids):
                window=next(w for w in inventory["windows"] if w["pid"]==pid and w["title"].startswith(_native._base.PREFIX))
                assert _ui.user32.SetWindowPos(window["hwnd"],wintypes.HWND(-1),30+i%2*460,40+i//2*410,420,360,0x10)
            inventory=client.tool("routing_windows")
            windows=[next(w for w in inventory["windows"] if w["pid"]==pid and w["title"].startswith(_native._base.PREFIX)) for pid in self.pids]
            self.points=[[w["client"]["left"]+120,w["client"]["top"]+100] for w in windows]
        finally: client.close()

    def output_ack(self,sample,value):
        state=read_json(self.run/"ui.output.json")
        logs=[json.loads(line) for line in (self.run/"ui.output.acks.jsonl").read_text().splitlines()]
        sample.update(output=state,output_acks=logs)
        return any(s["acknowledged"] and s["user"]==0 and not s["error"] and s["users"][0]["settings"]==value for s in logs) and state["users"][0]["settings"]==value

    def profile(self):
        return {"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":self.pids},
            "seats":[{"id":self.ids[i],"name":f"Owned output UI {i+1}","mouse":f"virtual://mouse/{i+1}","keyboard":f"virtual://keyboard/{i+1}","desktop_start":self.points[i],"cursor":{"lock_window":False}} for i in range(2)]}

    def controls(self):
        self.fixtures(); profile=self.profile()
        steps=[step("defaults-page","Profiles"),step("defaults-save","Save profile",{"profile_path":self.run/"defaults.json"}),step("lab","Input lab"),step("start","Start routing")]
        for i in range(2): steps += [step(f"select-{i}",f"{i+1} Hold click"),step(f"drop-{i}",f"{i+1} Release click")]
        steps += [step("other-hold","Hold key",{"selected_user":1,"key_name":"f6"}),step("output-page","Output Rules"),
            step("add-remap","Add or replace key mapping",{"output_user":0,"output_button":3,"output_kind":"key","output_key":"F5","output_source_key":"F7","output_destination_key":"F8"}),
            step("add-disabled","Add or replace key mapping",{"output_source_key":"F9","output_disable":"true"}),step("apply","Apply output rules"),step("apply-image","Apply output rules"),
            step("mapped-lab","Input lab"),step("mapped-down","1 Hold Back"),step("mapped-up","1 Release Back"),step("mapped-key","Tap key",{"selected_user":0,"key_name":"f7"}),step("disabled-key","Tap key",{"selected_user":0,"key_name":"f9"}),
            step("restore-page","Output Rules"),step("restore-buttons","Restore legacy mouse output"),step("apply-restored","Apply output rules"),step("restored-lab","Input lab"),
            step("legacy-down","1 Hold Back"),step("legacy-up","1 Release Back"),step("retained-key-map","Tap key",{"selected_user":0,"key_name":"f7"}),
            step("remove-page","Output Rules"),step("remove-rule","Remove key mapping F7"),step("apply-removed","Apply output rules"),step("stop","Stop routing"),
            step("save-rules","Save output rules to profile"),step("profile-page","Profiles"),step("save","Save profile",{"profile_path":self.run/"saved.json"}),
            step("reload","Load profile",{"profile_path":self.run/"saved.json"}),step("roundtrip","Save profile",{"profile_path":self.run/"roundtrip.json"})]
        mapped=_native.rules({3:116},{118:119,120:None}); restored=_native.rules(keys={118:119,120:None}); final=_native.rules(keys={120:None})
        baseline=None; overlays=[]; image_wall=None
        def verify(name,sample):
            nonlocal baseline,overlays,image_wall
            probes,state=sample["probes"],sample["state"]
            if name=="defaults-save":
                assert all(s["output"]==_native.rules() for s in read_json(self.run/"defaults.json")["seats"])
            if name=="start":
                if len(state)!=2:return False
                overlays=[u["overlay"]["hwnd"] for u in state]
            if name.startswith("select-") and not probes[int(name[-1])]["buttons"]&1:return False
            if name.startswith("drop-") and probes[int(name[-1])]["buttons"]:return False
            if name=="other-hold" and 117 not in probes[1]["held_keys"]:return False
            if name in ("apply","apply-image","apply-restored","apply-removed"):
                expected=mapped if name in ("apply","apply-image") else restored if name=="apply-restored" else final
                if not self.output_ack(sample,expected) or 117 not in probes[1]["held_keys"]:return False
                if name=="apply":
                    image_wall=image_wall or time.time(); png=self.run/"ui.png"
                    if not png.exists() or png.stat().st_mtime<image_wall:return False
                    with Image.open(png) as image:image.load();image.save(self.run/"output-controls.png")
                    sample["owned_output_screenshot"]=str(self.run/"output-controls.png")
            if name in ("mapped-lab","restored-lab"):baseline=_native._trace.NativeBaseline(probes)
            if name=="mapped-down":
                if 116 not in probes[0]["held_keys"]:return False
                baseline.expect_keys(probes,0,[(116,True)]);baseline.expect_no_button_delivery(probes,0)
            if name=="mapped-up":
                if 116 in probes[0]["held_keys"]:return False
                baseline.expect_keys(probes,0,[(116,True),(116,False)])
            if name in ("mapped-key","disabled-key"):
                expected=[(116,True),(116,False),(119,True),(119,False)]
                if len([e for e in baseline.new_events(probes,0) if e["kind"]=="key"])!=4:return False
                baseline.expect_keys(probes,0,expected);baseline.expect_no_button_delivery(probes,0)
                assert 117 in probes[1]["held_keys"]
            if name=="legacy-down" and probes[0]["buttons"]!=8:return False
            if name=="legacy-up":
                if probes[0]["buttons"]:return False
                assert [(e["code"],e["pressed"]) for e in baseline.new_events(probes,0) if e["kind"]=="button"]==[(3,True),(3,False)]
            if name=="retained-key-map":
                if len([e for e in baseline.new_events(probes,0) if e["kind"]=="key"])!=2:return False
                baseline.expect_keys(probes,0,[(119,True),(119,False)])
            if name=="stop":
                if any(p["held_keys"] or p["buttons"] for p in probes) or any(_ui.user32.IsWindow(h) for h in overlays):return False
                assert [(e["vk"],e["pressed"]) for e in probes[1]["key_events"]]==[(117,True),(117,False)],"Stop did not deliver a plain native F6 key-up"
            if name in ("save","roundtrip"):
                saved=read_json(self.run/("saved.json" if name=="save" else "roundtrip.json"))
                assert saved["seats"][0]["output"]==final and saved["seats"][1]["output"]==_native.rules()
                if name=="roundtrip":assert saved==read_json(self.run/"saved.json")
                sample["profile"]=saved
            return True
        self.execute(profile,steps,verify)

    def stop_tail(self):
        self.fixtures();profile=self.profile();profile["seats"][0]["output"]=_native.rules({3:116})
        steps=[step("lab","Input lab"),step("start","Start routing")]
        for i in range(2):steps += [step(f"select-{i}",f"{i+1} Hold click"),step(f"drop-{i}",f"{i+1} Release click")]
        steps += [step("converted-hold","1 Hold Back"),step("manual-hold","Hold key",{"selected_user":1,"key_name":"f6"}),step("stop","Stop routing")]
        overlays=[]
        def verify(name,sample):
            nonlocal overlays
            probes,state=sample["probes"],sample["state"]
            if name=="start":
                if len(state)!=2:return False
                overlays=[u["overlay"]["hwnd"] for u in state]
            if name.startswith("select-") and not probes[int(name[-1])]["buttons"]&1:return False
            if name.startswith("drop-") and probes[int(name[-1])]["buttons"]:return False
            if name=="converted-hold" and 116 not in probes[0]["held_keys"]:return False
            if name=="manual-hold" and (116 not in probes[0]["held_keys"] or 117 not in probes[1]["held_keys"]):return False
            if name=="stop":
                if any(p["held_keys"] or p["buttons"] for p in probes) or any(_ui.user32.IsWindow(h) for h in overlays):return False
                if any(p["key_event_count"]<2 for p in probes):return False
                for i,vk in enumerate((116,117)):
                    assert [(e["vk"],e["pressed"]) for e in probes[i]["key_events"]]==[(vk,True),(vk,False)],("Stop native key-up",i,probes[i]["key_events"])
            return True
        self.execute(profile,steps,verify)

    def find_descriptor(self):
        files=list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{self.owned[-1].pid}-*.json"))
        assert len(files)==1,"Expected one descriptor for own GUI PID"
        self.descriptor=files[0]; return self.descriptor

    def actor(self):
        result=_agents.OwnedPipe(self.descriptor);self.connections.append(result);return result

    @staticmethod
    def mouse_request(user,down,button=3):
        return {"Input":{"device":0xf1000002+user*2,"kind":0,"flags":1<<(button*2+(0 if down else 1)),"x":0,"y":0,"data":0}}

    def connected(self):
        self.fixtures();profile=self.profile();profile["seats"][0]["output"]=_native.rules({2:118,3:116,4:116});profile["seats"][1]["output"]=_native.rules({3:117})
        profile["seats"].append({"id":self.ids[2],"name":"Owned output no keyboard","mouse":"virtual://mouse/3","keyboard":None,"desktop_start":self.points[2],"cursor":{"lock_window":False}})
        grant={"agent_users":self.ids[0],"agent_pids":",".join(map(str,self.pids[:2]))}
        steps=[step("lab","Input lab"),step("start","Start routing")]
        for i in range(2):steps += [step(f"select-{i}",f"{i+1} Hold click"),step(f"drop-{i}",f"{i+1} Release click")]
        steps += [step("manual-a","Hold key",{"selected_user":0,"key_name":"f5"}),step("manual-b","Hold key",{"selected_user":1,"key_name":"f6"}),
            step("agent-page","Agent Access"),step("deny-and-owners","Enable agent access",grant),step("grant-output","Apply access policy",{**grant,"agent_session_control":"true"}),
            step("scope-lab","Input lab"),step("move-outside","1 Move to point",point=self.points[2],user=0),step("select-outside","1 Hold click"),step("drop-outside","1 Release click"),
            step("outside-manual","Hold key",{"selected_user":0,"key_name":"f8"}),step("mouse-hover-a","1 Move to point",point=self.points[0],user=0),
            step("guard-output","Agent Access"),step("restore-lab","Input lab"),step("restore-a","1 Hold click"),step("restore-a-up","1 Release click"),
            step("graph-page","Replication Graph"),step("delayed-edge","Connect selected users",{"graph_source":self.ids[0],"graph_target":self.ids[1],"graph_mouse":"true","graph_keyboard":"false","graph_text":"false","graph_motion":"false","graph_wheel":"false","graph_horizontal_wheel":"false","graph_button_primary":"false","graph_button_secondary":"false","graph_button_middle":"false","graph_button_back":"true","graph_button_forward":"false","graph_click_delay_ms":4000}),
            step("apply-delayed","Apply replication graph"),step("queue-manual-lab","Input lab"),step("queue-manual-owner","Hold key",{"selected_user":0,"key_name":"f5"}),step("allow-destination","Agent Access"),step("queue-converted","Apply access policy",{**grant,"agent_users":",".join(self.ids),"agent_session_control":"true"}),
            step("destination-lab","Input lab"),step("move-destination-outside","2 Move to point",point=self.points[2],user=1),step("select-destination-outside","2 Hold click"),step("delayed-scope-proof","2 Release click"),
            step("stop","Stop routing")]
        if self.args.connected_tail:
            profile["seats"][0]["output"]=_native.rules({3:118})
            indexed={s["id"]:s for s in steps}
            steps=[indexed[n] for n in ("lab","start","select-0","drop-0","select-1","drop-1")]
            steps += [step("enable-tail-policy","Agent Access"),step("enable-tail-grant","Enable agent access",{**grant,"agent_users":",".join(self.ids),"agent_session_control":"true"})]
            steps += [indexed[n] for n in ("graph-page","delayed-edge","apply-delayed","queue-manual-lab","queue-manual-owner","allow-destination","queue-converted","destination-lab","move-destination-outside","select-destination-outside","delayed-scope-proof","stop")]
        current=[None];overlays=[]
        def once(name,operation):
            if name not in self.once:self.once[name]=operation()
            return self.once[name]
        def verify(name,sample):
            nonlocal overlays
            probes,state=sample["probes"],sample["state"]
            if name=="start":
                if len(state)!=3:return False
                overlays=[u["overlay"]["hwnd"] for u in state]
            if name.startswith("select-") and name in ("select-0","select-1") and not probes[int(name[-1])]["buttons"]&1:return False
            if name.startswith("drop-") and name in ("drop-0","drop-1") and probes[int(name[-1])]["buttons"]:return False
            if name=="manual-a" and 116 not in probes[0]["held_keys"]:return False
            if name=="manual-b" and 117 not in probes[1]["held_keys"]:return False
            if name=="deny-and-owners":
                def phase():
                    self.find_descriptor(); actor=self.actor(); rejected=actor.request("execute",request={"Output":{"operation":"configure","user":0,"settings":_native.rules({3:117})}})
                    assert rejected["error"] and rejected["policy"]["session_control"] is False
                    baseline=_native._trace.NativeBaseline(self.probes())
                    assert not actor.request("execute",request=self.mouse_request(0,True))["error"]
                    assert not actor.request("execute",request=self.mouse_request(0,True,4))["error"]
                    assert not actor.request("execute",request=self.mouse_request(0,False))["error"]
                    assert not actor.request("execute",request="ReleaseAll")["error"]
                    actor.close();time.sleep(.2)
                    baseline.expect_keys(self.probes(),0,[]);baseline.expect_no_button_delivery(self.probes(),0);baseline.unchanged_holds(self.probes(),(0,1))
                    mcp=_agents.ConnectedMcp(self.images[0],self.descriptor,self.run/"connected.stderr.log");self.clients.append(mcp)
                    assert {t["name"] for t in mcp.rpc("tools/list",{})["tools"]}==_native.OutputGate.tool_names
                    mcp.tool("routing_output",{"operation":"configure","user":0,"settings":_native.rules({3:117})},error=True)
                    mcp.tool("routing_input",{"action":"button","user":0,"button":"back","pressed":True})
                    mcp.tool("routing_input",{"action":"button","user":0,"button":"middle","pressed":True})
                    eventually(self.probes,lambda rs:118 in rs[0]["held_keys"])
                    mcp.close();self.clients.remove(mcp)
                    eventually(self.probes,lambda rs:118 not in rs[0]["held_keys"])
                    baseline.expect_keys(self.probes(),0,[(118,True),(118,False)]);baseline.unchanged_holds(self.probes(),(0,1))
                    return {"raw_denial":rejected["error"],"actor_release_disconnect_and_mcp_eof_preserved_manual_union":True,"probes":self.probes()}
                sample["agent_proof"]=once(name,phase)
            if name=="grant-output":
                def phase():
                    actor=self.actor();current[0]=actor;result=actor.request("execute",request={"Output":{"operation":"configure","user":0,"settings":_native.rules({3:118})}})
                    assert not result["error"] and result["profile"]["seats"][0]["output"]==_native.rules({3:118})
                    denied=actor.request("execute",request={"Output":{"operation":"configure","user":1,"settings":_native.rules({3:119})}});assert denied["error"]
                    baseline=_native._trace.NativeBaseline(self.probes())
                    assert not actor.request("execute",request=self.mouse_request(0,True))["error"]
                    eventually(self.probes,lambda rs:118 in rs[0]["held_keys"])
                    assert not actor.request("execute",request=self.mouse_request(0,False))["error"]
                    eventually(self.probes,lambda rs:118 not in rs[0]["held_keys"])
                    baseline.expect_keys(self.probes(),0,[(118,True),(118,False)]);assert 116 in self.probes()[0]["held_keys"] and 117 in self.probes()[1]["held_keys"]
                    return {"configure_ack_client":result["client"],"configure_ack_sequence":result["sequence"],"other_user_denied":denied["error"],"probes":self.probes()}
                sample["agent_proof"]=once(name,phase)
            if name=="outside-manual" and 119 not in probes[2]["held_keys"]:return False
            if name=="queue-manual-owner" and 116 not in probes[0]["held_keys"]:return False
            if name=="guard-output":
                def phase():
                    baseline=_native._trace.NativeBaseline(self.probes());actor=current[0]
                    rejected=actor.request("execute",request=self.mouse_request(0,True));assert rejected["error"]
                    # A valid replacement may configure an unfocused owner, but cannot
                    # inject or retire an unrelated native hold outside the PID scope.
                    time.sleep(.2);baseline.expect_keys(self.probes(),2,[]);assert 119 in self.probes()[2]["held_keys"]
                    return {"native_keyboard_recipient_rejection":rejected["error"],"outside_hold_preserved":True,"probes":self.probes()}
                sample["agent_proof"]=once(name,phase)
            if name=="queue-converted":
                def phase():
                    if current[0]:current[0].close()
                    if not self.descriptor:self.find_descriptor()
                    current[0]=self.actor()
                    self.queue_baseline=_native._trace.NativeBaseline(self.probes())
                    for candidate in (_native.rules({3:116}),_native.rules(keys={120:None})):
                        rejected=current[0].request("execute",request={"Output":{"operation":"configure","user":2,"settings":candidate}})
                        assert rejected["error"] and "keyboard" in rejected["error"].lower(),rejected["error"]
                    self.queue_baseline.unchanged_holds(self.probes(),(0,1,2))
                    result=current[0].request("execute",request=self.mouse_request(0,True));assert not result["error"],result["error"]
                    pending=[e["Automation"]["replication"]["pending_events"] for e in result["events"] if isinstance(e,dict) and "Automation" in e]
                    assert pending and pending[-1]>0,"Converted replica was not actually queued"
                    eventually(self.probes,lambda rs:118 in rs[0]["held_keys"] and 116 in rs[0]["held_keys"])
                    return {"client":result["client"],"sequence":result["sequence"],"submitted_monotonic":time.monotonic(),"keyboardless_key_and_disable_rejections_preserved_holds":True}
                sample["agent_proof"]=once(name,phase)
            if name=="delayed-scope-proof":
                def phase():
                    # Wait beyond the real 4-second queue delay without changing policy.
                    deadline=self.once["queue-converted"]["submitted_monotonic"]+5
                    while time.monotonic()<deadline:time.sleep(.05)
                    self.queue_baseline.expect_keys(self.probes(),2,[])
                    state=current[0].request("inspect")
                    events=state["events"]
                    routing=next(e["State"] for e in events if isinstance(e,dict) and "State" in e)
                    queue=next(e["Automation"]["replication"] for e in events if isinstance(e,dict) and "Automation" in e)
                    assert routing[1]["keyboard_pid"]==self.pids[2],"Destination keyboard did not actually change PID"
                    assert queue["pending_events"]==0,"Delayed job did not actually finish/cancel"
                    assert 116 in self.probes()[0]["held_keys"] and 118 not in self.probes()[0]["held_keys"],"Async scope cancellation did not preserve the manual owner"
                    current[0].request("execute",request=self.mouse_request(0,False))
                    return {"outside_native_key_edges":[],"routing_remained_live":True,"events":events,"probes":self.probes()}
                sample["agent_proof"]=once(name,phase)
            if name=="stop":
                if any(p["held_keys"] or p["buttons"] for p in probes) or any(_ui.user32.IsWindow(h) for h in overlays):return False
                if self.descriptor and self.descriptor.exists():return False
            return True
        self.execute(profile,steps,verify)

    def close(self):
        for connection in self.connections:connection.close()
        for client in self.clients:
            try:client.close()
            except Exception:client.process.kill();client.process.wait(timeout=10)
        for process in reversed(self.owned):
            if process.poll() is None:process.terminate()
            process.wait(timeout=15)
        # Failed forced GUI termination can leave its descriptor. Remove only
        # our recorded descriptor after proving its owner process has exited.
        if self.descriptor and self.descriptor.exists():
            owner=json.loads(self.descriptor.read_text())["controller_pid"]
            assert any(p.pid==owner and p.poll() is not None for p in self.owned)
            self.descriptor.unlink();self.evidence["runner_removed_owned_stale_descriptor"]=True
        self.evidence["owned_processes_closed"]=all(p.poll() is not None for p in self.owned)
        self.save()

def main():
    parser=argparse.ArgumentParser()
    for name in ("executable","x86-executable","runtime"):parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--ready",action="store_true");parser.add_argument("--connected",action="store_true");parser.add_argument("--connected-tail",action="store_true");parser.add_argument("--stop-only",action="store_true")
    args=parser.parse_args();assert args.ready,"No native launch before producer READY"
    gate=OutputUi(args)
    try:
        assert not (args.connected and args.stop_only)
        if args.stop_only:gate.stop_tail()
        elif args.connected or args.connected_tail:gate.connected()
        else:gate.controls()
    finally:gate.close()

if __name__=="__main__":main()
