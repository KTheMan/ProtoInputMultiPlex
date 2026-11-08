"""Bounded connected Studio integration gate; requires frozen READY/exclusive slot.

Only app-local virtual input and owned probes. No native execution at import.
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


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def event(at, action):
    return {"at_micros": at, "action": action}


def key(user, pressed, vk=116, scan=63):
    return {"type":"packet","user":user,"packet":{"device":0xF1000003+2*user,
        "kind":1,"flags":0 if pressed else 1,"x":scan,"y":0,"data":vk}}


def button(user, pressed, index=3):
    return {"type":"packet","user":user,"packet":{"device":0xF1000002+2*user,
        "kind":0,"flags":1 << (2*index+(0 if pressed else 1)),"x":0,"y":0,"data":0}}


def clip(*events):
    return {"version":1,"events":list(events)}


def workflow(args):
    lifecycle = module("connected_studio_lifecycle", "test-app-lifecycle-ui.py")
    transport = module("connected_studio_transport", "test-connected-agents-acceptance.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    ids = ["connected-studio-a", "connected-studio-b"]

    class Gate(Base):
        def __init__(self):
            super().__init__(args,"connected-macro-studio",offline=False,count=2)
            self.clients, self.raw_clients = [], []
            self.mcp = self.raw = self.descriptor = None
            self.frames, self.frame_started, self.checks = {}, {}, {}
            self.layers, self.expected_profile = set(), None
            self.last_clip = None
            self.permitted = set()

        def probes(self):
            return ui.eventually(lambda:super(Gate,self).probes(),lambda reports:True,3)

        def snapshot(self):
            return ui.eventually(lambda:super(Gate,self).snapshot(),lambda state:True,3)

        def fixture(self):
            geometry.physical_pixel_context()
            for i in range(2):
                self.owned.append(subprocess.Popen([str(self.images[0]),"probe","--automation",
                    "--report",str(self.run/f"probe-{i}.json")],creationflags=subprocess.CREATE_NO_WINDOW))
            self.pids = [p.pid for p in self.owned]
            def windows():
                found = {}
                @geometry.callback_type
                def visit(hwnd,unused):
                    pid = wintypes.DWORD()
                    geometry.user32.GetWindowThreadProcessId(hwnd,ctypes.byref(pid))
                    if pid.value in self.pids and not geometry.user32.GetWindow(hwnd,4):
                        title = ctypes.create_unicode_buffer(80)
                        geometry.user32.GetWindowTextW(hwnd,title,len(title))
                        if title.value.startswith("ProtoInputMultiPlex Automation Test"):
                            found[pid.value] = int(hwnd)
                    return True
                assert geometry.user32.EnumWindows(visit,0)
                return found
            found = ui.eventually(windows,lambda result:len(result)==2,20)
            geometry.user32.ClientToScreen.argtypes = [wintypes.HWND,ctypes.POINTER(wintypes.POINT)]
            for i,pid in enumerate(self.pids):
                hwnd = found[pid]
                assert geometry.user32.SetWindowPos(hwnd,wintypes.HWND(-1),30+460*i,40,420,360,0x10)
                point = wintypes.POINT(120,100)
                assert geometry.user32.ClientToScreen(hwnd,ctypes.byref(point))
                self.points.append([point.x,point.y])

        def new_mcp(self):
            self.mcp = transport.ConnectedMcp(self.images[0],self.descriptor,
                self.run/f"mcp-{len(self.clients)}.stderr.log")
            self.clients.append(self.mcp)
            return self.mcp

        def reconnect(self):
            for client in self.clients:
                if client.process.poll() is None:
                    client.close()
            for client in self.raw_clients:
                client.close()
            paths = list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{self.gui.pid}-*.json"))
            assert len(paths)==1,"Discover only own GUI PID descriptor"
            self.descriptor = paths[0]
            self.new_mcp()
            self.raw = transport.OwnedPipe(self.descriptor)
            self.raw_clients.append(self.raw)
            result = self.inspect()
            self.permitted = set(result["policy"]["user_ids"])
            return result

        def project(self,result):
            events = result["events"]
            for event_value in events:
                if isinstance(event_value,dict) and "State" in event_value:
                    self.layers.update(u["overlay"]["hwnd"] for u in event_value["State"] if u.get("overlay"))
            if self.expected_profile is not None:
                assert result["profile"]==self.expected_profile,"Macro command changed profile settings"
            return [e["MacroStudio"] for e in events if isinstance(e,dict) and "MacroStudio" in e][-1]

        def inspect(self):
            result = self.raw.request("inspect")
            self.project(result)
            return result

        def raw_studio(self,command,ack=False):
            sequence = self.raw.sequence
            result = self.raw.request("execute",request={"MacroStudio":command})
            assert result["client"]==self.raw.client and result["sequence"]==sequence and not result.get("error")
            state = self.project(result)
            if ack:
                actual = [e["MacroStudio"] for e in result["events"] if isinstance(e,dict)
                    and "MacroStudio" in e and e["MacroStudio"]["acknowledged"]]
                assert len(actual)==1 and not actual[0].get("error")
                state = actual[0]
            return state

        def lane(self,user):
            # Explicit Inspect(user) is scoped; the ordinary periodic snapshot
            # has all lane metadata but omits unchanged clips. Do not bypass it.
            if ids[user] not in self.permitted:
                return next(u for u in self.project(self.inspect())["users"] if u["user"]==user)
            state = self.raw_studio({"operation":"inspect","user":user})
            return next(u for u in state["users"] if u["user"]==user)

        def command(self,command,error=False):
            result = self.mcp.tool("macro_studio",command,error=error)
            if not error:
                assert result["macro_studio"]["acknowledged"] and not result["macro_studio"].get("error")
            return result

        def signature(self):
            lanes = []
            for i in range(2):
                user = self.lane(i)
                lanes.append({k:user.get(k) for k in
                    ("user","recording","recorded_events","loaded_events","revision","clip","agent_client")})
                lanes[-1]["playback_state"] = (user.get("playback") or {}).get("state")
            holds = [{"keys":r["held_keys"],"buttons":r["buttons"],"text":r["text"]} for r in self.probes()]
            return {"lanes":lanes,"holds":holds,"profile":self.inspect()["profile"]}

        def deny(self,name,command):
            before = self.signature()
            result = self.command(command,error=True)
            assert self.signature()==before,"Rejected Studio mutation changed lane/native claims"
            self.record(name,{"error":result,"lanes_holds_profile_unchanged":True})

        def record(self,name,value):
            assert name not in self.checks
            self.checks[name] = value
            self.evidence["checks"] = self.checks
            self.save()

        def input(self,action,actor=None):
            if actor is None:
                return self.mcp.tool("routing_input",action)
            kind = action["action"]
            user = action["user"]
            if kind=="key":
                vk = {"f5":116,"f7":118}[action["key"]]
                packet = key(user,action["pressed"],vk,63 if vk==116 else 65)["packet"]
                request = {"Input":packet}
            elif kind=="move_to":
                request = {"MoveTo":{k:action[k] for k in ("user","x","y")}}
            else:
                assert kind=="button" and action["button"]=="primary"
                request = {"Input":button(user,action["pressed"],0)["packet"]}
            result = actor.request("execute",request=request)
            assert not result.get("error"),result.get("error")
            return result

        def focus(self,user,target,actor=None):
            x,y = self.points[target]
            self.input({"action":"move_to","user":user,"x":x,"y":y},actor)
            for pressed in (True,False):
                self.input({"action":"button","user":user,"button":"primary","pressed":pressed},actor)

        def native(self,predicate):
            return ui.eventually(self.probes,predicate,10)

        def gui_b_held(self):
            return 117 in self.probes()[1]["held_keys"] and self.lane(1).get("agent_client") is None

        def key_edges(self,user,vk):
            return [e["pressed"] for e in self.probes()[user]["key_events"] if e["vk"]==vk]

        def play(self):
            result = self.command({"operation":"play","user":0,"options":{
                "loops":1,"speed":1.0,"delay_ms":0,"stop_on_escape":True,"keep_awake":False}})
            self.native(lambda r:116 in r[0]["held_keys"])
            lane = self.lane(0)
            assert lane["agent_client"] is not None and lane["playback"]["state"]=="playing"
            return result

        def frame(self,name):
            if name in self.frames:
                return True
            hwnd = self.main_hwnd()
            if name not in self.frame_started:
                geometry.resize_owned_client(hwnd,[680,620])
                self.frame_started[name] = time.time()
                return False
            shot = geometry.copy_owned_product_frame(self.run/"ui.png",self.run/(name+".png"),
                hwnd,self.frame_started[name])
            if not shot:
                return False
            self.frames[name] = shot
            return True

        def agent_acks(self):
            path = self.run/"ui.agent.acks.jsonl"
            return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()] if path.exists() else []

        def studio_acks(self):
            path = self.run/"ui.macro-studio.acks.jsonl"
            return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()] if path.exists() else []

    g = Gate()
    prefs = {"version":1,"appearance_mode":"dark","zoom_percent":100,
        "launch_minimized":False,"minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,"shortcuts":[]}
    complete = clip(event(0,{"type":"text","user":0,"text":"CM✓"}),event(10000,key(0,True)),
        event(20000,key(0,False)),event(30000,button(0,True)),event(40000,button(0,False)))
    hold = clip(event(0,key(0,True)),event(10000,button(0,True)),
        event(30000000,button(0,False)),event(30010000,key(0,False)))
    pinned = clip(event(0,key(0,True)),event(2000000,key(0,False)),
        event(2010000,key(0,True,119,66)),event(2020000,key(0,False,119,66)))
    phases_done = set()
    expected_clip = None
    gui_clip_path = g.run/"gui-b.json"
    saved_agent_clip = g.run/"gui-cached-agent-a.json"

    def permission_off():
        g.reconnect()
        g.expected_profile = copy.deepcopy(g.inspect()["profile"])
        before = g.signature()
        result = g.mcp.tool("macro_studio",{"operation":"inspect","user":0})
        assert result["macro_studio"]["users"][0]["revision"]==0
        assert g.signature()==before
        g.deny("no_control_authoring",{"operation":"load","user":0,"clip":complete})

    def authoring():
        nonlocal expected_clip
        state = g.reconnect()
        assert state["policy"]["session_control"] and state["policy"]["user_ids"]==ids[:1]
        for active in (True,False):
            g.deny("record_unavailable_"+str(active),{"operation":"record","user":0,"active":active})
        first = g.command({"operation":"load","user":0,"clip":complete})["macro_studio"]
        revision = first["users"][0]["revision"]
        edited = g.raw_studio({"operation":"edit","user":0,"edit":{
            "operation":"retime","index":4,"at_micros":50000}},ack=True)
        expected = copy.deepcopy(complete);expected["events"][4]["at_micros"] = 50000
        assert edited["users"][0]["clip"]==expected and edited["users"][0]["revision"]==revision+1
        g.record("authoring_load_edit_tagged_ack",{"load":first,"edit":edited})
        generator = {"shape":"line","user":0,"center":g.points[0],"duration_micros":10000,
            "radius":2,"points":2,"click":None,"seed":4}
        generated = g.command({"operation":"generate","user":0,"settings":generator})["macro_studio"]["users"][0]
        assert generated["loaded_events"]==3 and all(e["action"]["user"]==0 for e in generated["clip"]["events"])
        g.command({"operation":"reset","user":0})
        assert g.lane(0)["clip"] is None
        g.command({"operation":"load","user":0,"clip":complete})
        bad = copy.deepcopy(complete);bad["version"] = 99
        g.deny("invalid_clip",{"operation":"load","user":0,"clip":bad})
        g.deny("wrong_user_edit",{"operation":"edit","user":0,"edit":{
            "operation":"insert","event":event(0,key(1,True))}})
        g.deny("unknown_user",{"operation":"reset","user":99})
        g.deny("unpermitted_destination",{"operation":"edit","user":0,"edit":{
            "operation":"remap","from":0,"to":1}})
        g.deny("unpermitted_source",{"operation":"edit","user":1,"edit":{
            "operation":"remap","from":1,"to":0}})
        g.record("generate_reset_restore",{"generated":generated,"restored":g.lane(0)})
        g.focus(0,0)
        g.command({"operation":"play","user":0,"options":{"loops":1,"keep_awake":False}})
        reports = g.native(lambda r:r[0]["text"]=="CM✓" and not r[0]["held_keys"] and not r[0]["buttons"]
            and g.lane(0).get("playback",{}).get("state")=="completed")
        assert g.key_edges(0,116)==[True,False] and reports[1]["text"]==""
        assert [e["pressed"] for e in reports[0]["mouse_events"] if e["button"]==3]==[True,False]
        g.record("native_completion_text_key_button",{"reports":reports,"lane":g.lane(0)})
        g.command({"operation":"load","user":0,"clip":hold})
        g.input({"action":"key","user":0,"key":"f7","pressed":True},g.raw)
        g.native(lambda r:118 in r[0]["held_keys"])
        g.deny("play_preexisting_claim_rejected",{"operation":"play","user":0,"options":{"keep_awake":False}})
        g.input({"action":"key","user":0,"key":"f7","pressed":False},g.raw)
        g.native(lambda r:not r[0]["held_keys"])
        expected_clip = copy.deepcopy(hold)

    def ownership():
        assert g.gui_b_held()
        baseline = len(g.key_edges(0,116))
        g.play()
        g.native(lambda r:r[0]["buttons"]&8 and 116 in r[0]["held_keys"])
        g.input({"action":"key","user":0,"key":"f5","pressed":True},g.raw)
        g.deny("own_playing_authoring_rejected",{"operation":"reset","user":0})
        sequence = g.raw.sequence
        foreign = g.raw.request("execute",request={"MacroStudio":{"operation":"cancel","user":0}})
        assert foreign["client"]==g.raw.client and foreign["sequence"]==sequence and foreign.get("error")
        assert g.lane(0)["playback"]["state"]=="playing"
        g.command({"operation":"cancel","user":0})
        reports = g.native(lambda r:116 in r[0]["held_keys"] and not r[0]["buttons"])
        assert len(g.key_edges(0,116))==baseline+1 and g.gui_b_held()
        g.input({"action":"key","user":0,"key":"f5","pressed":False},g.raw)
        g.native(lambda r:not r[0]["held_keys"])
        assert g.key_edges(0,116)[baseline:]==[True,False]
        assert g.lane(0)["clip"]==hold
        g.record("scoped_cancel_first_down_last_up",{"while_other_actor_held":reports,"gui_b_held":True,
            "native_f5_edges":g.key_edges(0,116)[baseline:]})
        g.command({"operation":"load","user":0,"clip":pinned})
        g.play()
        # No Studio mouse hold here: a second allowed actor can genuinely change
        # selection without GUI manual override cancelling the lane first.
        g.focus(0,1,g.raw)
        reports = g.native(lambda r:116 not in r[0]["held_keys"])
        ui.eventually(lambda:g.lane(0),lambda lane:lane.get("agent_client") is None,10)
        assert not g.key_edges(1,119) and g.gui_b_held()
        g.record("pinned_target_change",{"reports":reports,"lane":g.lane(0),"no_f8_on_new_target":True})
        g.focus(0,0,g.raw)
        g.command({"operation":"load","user":0,"clip":hold})
        g.play()
        g.input({"action":"key","user":0,"key":"f7","pressed":True},g.raw)
        g.native(lambda r:118 in r[0]["held_keys"])
        owner = g.lane(0)["agent_client"]
        g.mcp.close()
        reports = g.native(lambda r:116 not in r[0]["held_keys"] and 118 in r[0]["held_keys"] and not r[0]["buttons"])
        assert g.gui_b_held() and g.lane(0)["clip"]==hold
        g.input({"action":"key","user":0,"key":"f7","pressed":False},g.raw)
        g.native(lambda r:not r[0]["held_keys"])
        g.new_mcp()
        g.record("owner_disconnect_preserves_other_actor_gui",{"disconnected_owner":owner,"reports":reports,
            "clip_retained":True,"gui_b_held":True})

    def arm_revocation():
        assert g.lane(0)["clip"]==expected_clip and g.gui_b_held()
        g.play()
        g.native(lambda r:116 in r[0]["held_keys"] and r[0]["buttons"]&8)
        g.record("armed_policy_revocation",{"lane":g.lane(0),"reports":g.probes()})

    def revoked():
        reports = g.native(lambda r:116 not in r[0]["held_keys"] and not r[0]["buttons"] and 117 in r[1]["held_keys"])
        state = g.reconnect()
        assert set(state["policy"]["user_ids"])==set(ids) and state["policy"]["allowed_pids"]==g.pids[1:]
        assert g.lane(0)["clip"]==hold and g.gui_b_held()
        g.deny("play_actual_pid_not_granted",{"operation":"play","user":0,"options":{"keep_awake":False}})
        for operation in ("load","play","cancel"):
            command = {"operation":operation,"user":1}
            if operation=="load":command["clip"] = complete
            if operation=="play":command["options"] = {"keep_awake":False}
            g.deny("gui_owned_lane_"+operation,command)
        g.record("policy_revocation_preserves_gui_lane",{"reports":reports,"clip_retained":True})
        for client in g.clients:
            if client.process.poll() is None:client.close()
        for client in g.raw_clients:client.close()

    def verify(name,sample):
        if g.expected_profile is not None:
            assert g.snapshot()["profile"]==g.expected_profile
        if name in {"enable","grant","revoke"}:
            required = {"enable":1,"grant":2,"revoke":3}[name]
            acks = g.agent_acks()
            if len(acks)<required:return False
            ack = acks[-1]
            assert len(acks)==required and not ack["pending"] and not ack["error"] and ack["policy_enabled"]
            assert ack["session_control"]==(name!="enable") and not ack["capture"] and not ack["app_preferences"]
            if name not in phases_done:
                {"enable":permission_off,"grant":authoring,"revoke":revoked}[name]()
                phases_done.add(name)
            if name=="grant" and not g.frame("permissions-authoring"):return False
        if name=="load-b":
            lane = g.lane(1)
            acks = g.studio_acks()
            matching = [user for ack in acks if ack["acknowledged"] and not ack.get("error")
                for user in ack["users"] if user["user"]==1 and user.get("clip")==gui_b_clip]
            if not matching:return False
            assert lane["loaded_events"]==len(gui_b_clip["events"]) and lane["revision"]==matching[-1]["revision"]
            assert lane["agent_client"] is None
        if name=="play-b":
            g.native(lambda r:117 in r[1]["held_keys"])
            if name not in phases_done:
                ownership();phases_done.add(name)
            if not g.frame("gui-owned-b-playback"):return False
        if name=="save-a":
            if not saved_agent_clip.exists():return False
            saved = ui.eventually(lambda:ui.read_json(saved_agent_clip),lambda value:True,3)
            assert saved==expected_clip==g.lane(0)["clip"],"GUI cache lost connected authored clip"
            if name not in phases_done:
                g.record("gui_cached_acknowledged_clip",{"saved_path":str(saved_agent_clip),
                    "sha256":hashlib.sha256(saved_agent_clip.read_bytes()).hexdigest(),"lane":g.lane(0)})
                arm_revocation();phases_done.add(name)
            if not g.frame("agent-owned-a-cache"):return False
        if name=="stop":
            if any(ui.user32.IsWindow(hwnd) for hwnd in g.layers) or (g.descriptor and g.descriptor.exists()):return False
            reports = g.probes()
            if any(r["held_keys"] or r["buttons"] for r in reports):return False
            assert g.key_edges(1,117)==[True,False],"GUI B hold did not receive its plain native UP on Stop"
            sample["plain_gui_b_f6_pair"] = g.key_edges(1,117)
        return True

    def after(gate):
        ui.put(gate.run/"final-profile.json",gate.snapshot()["profile"])
        gate.expected_exit = True
        assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0)
        gate.gui.wait(timeout=10)
        assert gate.gui.returncode==0 and not hotkeys.lifecycle_window(gate.gui.pid)
        return True

    try:
        g.fixture()
        x,y = g.points[1]
        gui_b_clip = clip(event(0,{"type":"move_to","user":1,"x":x,"y":y}),
            event(10000,button(1,True,0)),event(20000,button(1,False,0)),
            event(30000,key(1,True,117,64)),event(300000000,key(1,False,117,64)))
        ui.put(gui_clip_path,gui_b_clip)
        profile = {"version":1,"concurrent":True,"desktop":{"mode":"concurrent",
            "multi_keyboard":True,"allowed_pids":g.pids},"seats":[{"id":ids[i],
                "name":"Owned Studio "+("Alpha" if i==0 else "Beta"),"mouse":f"virtual://mouse/{i+1}",
                "keyboard":f"virtual://keyboard/{i+1}","desktop_start":g.points[i],
                "cursor":{"lock_window":False}} for i in range(2)]}
        step = ui.step
        steps = [step("start","Start routing"),step("settings","Settings"),
            step("category","Settings category Agent access"),step("agent","Agent Access"),
            step("enable","Enable agent access",{"agent_users":ids[0],"agent_pids":','.join(map(str,g.pids)),
                "agent_session_control":"false","agent_capture":"false","agent_app_preferences":"false"}),
            step("control","Agent session control permission"),step("grant","Apply access policy"),
            step("home-b","Back to Home"),step("select-b","Select user 2"),step("configure-b","Configure user"),
            step("macros-b","Macros"),step("load-b","Load user clip JSON",{"studio_file_path":str(gui_clip_path)}),
            step("play-b","Play user macro",{"studio_loops":1,"studio_repeat":"false","studio_speed":1,
                "studio_delay_ms":0,"studio_keep_awake":"false"}),step("home-a","Back to Home"),
            step("select-a","Select user 1"),step("configure-a","Configure user"),step("macros-a","Macros"),
            step("save-a","Save user clip JSON",{"studio_file_path":str(saved_agent_clip)}),
            step("home-revoke","Back to Home"),step("settings-revoke","Settings"),step("agent-revoke","Agent Access"),
            step("revoke","Apply access policy",{"agent_users":','.join(ids),"agent_pids":str(g.pids[1])}),
            step("home-stop","Back to Home"),step("stop","Stop routing")]
        assert len(steps)==24 and not any({"studio_user","selected_user"}&set(s.get("fields",{})) for s in steps)
        provenance = g.run/"runner-source";provenance.mkdir()
        for name in (Path(__file__).name,"test-connected-agents-acceptance.py","owned-gui-geometry.py"):
            shutil.copy2(Path(__file__).with_name(name),provenance/name)
        g.evidence["scope"] = "Connected Studio authoring/ACK/cache/ownership/target pin/revocation only; no recording, generator/queued/full-family replay or i686 execution"
        g.execute(profile,prefs,steps,verify,lambda gate:gate.main_hwnd() is not None,after)
        g.evidence.update(result="PENDING INDEPENDENT VISUAL REVIEW",control_state_result="PASS",captures=g.frames)
        g.save()
        return g.run
    finally:
        for client in g.clients:
            if client.process.poll() is None:client.close()
        for client in g.raw_clients:client.close()
        g.evidence["connected_mcp_closed"] = all(c.process.poll() is not None for c in g.clients)
        g.evidence["raw_handles_closed"] = all(c.handle is None for c in g.raw_clients)
        g.close();lifecycle.remove_startup_key(g.startup_key)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready",action="store_true")
    for name in ("executable","x86-executable","runtime"):
        parser.add_argument("--"+name,type=Path,required=True)
    args = parser.parse_args()
    assert args.ready,"Await final frozen READY/exclusive slot"
    workflow(args)


if __name__=="__main__":
    main()
