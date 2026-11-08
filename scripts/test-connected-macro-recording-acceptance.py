"""Client-attributed recording gate: own virtual inputs only, frozen READY required."""
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


def workflow(args):
    lifecycle = module("record_lifecycle", "test-app-lifecycle-ui.py")
    transport = module("record_transport", "test-connected-agents-acceptance.py")
    authored = module("record_actions", "test-connected-macro-studio-acceptance.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    ids = ["record-owned-a", "record-owned-b"]

    class Gate(Base):
        def __init__(self):
            super().__init__(args,"connected-macro-recording",offline=False,count=2)
            self.clients, self.raw_clients = [], []
            self.mcp = self.raw = self.descriptor = None
            self.frames, self.started_frames, self.checks = {}, {}, {}
            self.layers, self.permitted = set(), set()
            self.profile_expected = None
            self.last_raw_ack = None

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
            found = ui.eventually(windows,lambda value:len(value)==2,20)
            geometry.user32.ClientToScreen.argtypes = [wintypes.HWND,ctypes.POINTER(wintypes.POINT)]
            for i,pid in enumerate(self.pids):
                hwnd = found[pid]
                assert geometry.user32.SetWindowPos(hwnd,wintypes.HWND(-1),30+460*i,40,420,360,0x10)
                point = wintypes.POINT(120,100)
                assert geometry.user32.ClientToScreen(hwnd,ctypes.byref(point))
                self.points.append([point.x,point.y])

        def new_owner(self):
            self.mcp = transport.ConnectedMcp(self.images[0],self.descriptor,
                self.run/f"mcp-{len(self.clients)}.stderr.log")
            self.clients.append(self.mcp)

        def reconnect(self):
            for client in self.clients:
                if client.process.poll() is None:client.close()
            for client in self.raw_clients:client.close()
            paths = list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{self.gui.pid}-*.json"))
            assert len(paths)==1,"Find only own GUI descriptor"
            self.descriptor = paths[0]
            self.new_owner()
            self.raw = transport.OwnedPipe(self.descriptor)
            self.raw_clients.append(self.raw)
            result = self.inspect()
            self.permitted = set(result["policy"]["user_ids"])
            return result

        def project(self,result):
            if self.profile_expected is not None:
                assert result["profile"]==self.profile_expected,"Recording changed profile settings"
            for e in result["events"]:
                if isinstance(e,dict) and "State" in e:
                    self.layers.update(u["overlay"]["hwnd"] for u in e["State"] if u.get("overlay"))
            return [e["MacroStudio"] for e in result["events"] if isinstance(e,dict) and "MacroStudio" in e][-1]

        def inspect(self):
            result = self.raw.request("inspect")
            self.project(result)
            return result

        def lane(self,user=0):
            if ids[user] not in self.permitted:
                return next(u for u in self.project(self.inspect())["users"] if u["user"]==user)
            result = self.raw.request("execute",request={"MacroStudio":{"operation":"inspect","user":user}})
            assert not result.get("error")
            return next(u for u in self.project(result)["users"] if u["user"]==user)

        def studio(self,command,error=False):
            result = self.mcp.tool("macro_studio",command,error=error)
            if not error:
                assert result["macro_studio"]["acknowledged"] and not result["macro_studio"].get("error")
            return result

        def record(self,name,value):
            assert name not in self.checks
            self.checks[name] = value
            self.evidence["checks"] = self.checks
            self.save()

        def signature(self):
            lanes = []
            for i in range(2):
                lane = self.lane(i)
                lanes.append({k:lane.get(k) for k in ("user","clip","revision","loaded_events",
                    "recording","recorded_events","recording_client","recording_error")})
            return {"lanes":lanes,
                "holds":[{"keys":r["held_keys"],"buttons":r["buttons"],"text":r["text"],
                    "key_events":r["key_events"],"mouse_events":r["mouse_events"],"text_events":r["text_events"]}
                    for r in self.probes()],
                "profile":self.inspect()["profile"]}

        def deny_studio(self,name,command,foreign=False):
            before = self.signature()
            if foreign:
                sequence = self.raw.sequence
                result = self.raw.request("execute",request={"MacroStudio":command})
                assert result["client"]==self.raw.client and result["sequence"]==sequence and result.get("error")
            else:
                result = self.studio(command,error=True)
            assert self.signature()==before,"Rejected command changed candidate, committed clip or input"
            self.record(name,{"reply":result,"unchanged":True})

        def owner_input(self,action,error=False):
            return self.mcp.tool("routing_input",action,error=error)

        def actor_input(self,request):
            sequence = self.raw.sequence
            reply = self.raw.request("execute",request=request)
            assert reply["client"]==self.raw.client and reply["sequence"]==sequence and not reply.get("error")
            return reply

        def actor_key(self,pressed,vk=116,scan=63):
            before = len([e for e in self.probes()[0]["key_events"] if e["vk"]==vk])
            result = self.actor_input({"Input":authored.key(0,pressed,vk,scan)["packet"]})
            self.native(lambda r:len([e for e in r[0]["key_events"] if e["vk"]==vk])==before+1
                and ((vk in r[0]["held_keys"])==pressed))
            return result

        def focus(self,user,target,actor=False):
            x,y = self.points[target]
            before = len([e for e in self.probes()[target]["mouse_events"] if e["button"]==0])
            if actor:
                self.actor_input({"MoveTo":{"user":user,"x":x,"y":y}})
                for pressed in (True,False):
                    self.actor_input({"Input":authored.button(user,pressed,0)["packet"]})
            else:
                self.owner_input({"action":"move_to","user":user,"x":x,"y":y})
                for pressed in (True,False):
                    self.owner_input({"action":"button","user":user,"button":"primary","pressed":pressed})
            self.native(lambda r:len([e for e in r[target]["mouse_events"] if e["button"]==0])==before+2
                and not r[target]["buttons"]&1)

        def native(self,predicate):
            return ui.eventually(self.probes,predicate,10)

        def start_recording(self):
            result = self.studio({"operation":"record","user":0,"active":True})
            lane = self.lane()
            assert lane["recording"] and lane["recording_client"] is not None and lane["recorded_events"]==0
            assert lane["clip"]==committed["clip"] and lane["revision"]==committed["revision"]
            return result

        def working(self,count):
            lane = ui.eventually(self.lane,lambda u:u["recording"] and u["recorded_events"]==count,5)
            assert lane["recording_client"] is not None
            assert lane["clip"]==committed["clip"] and lane["revision"]==committed["revision"]
            return lane

        def prior_preserved(self):
            lane = ui.eventually(self.lane,lambda u:not u["recording"] and u["recording_client"] is None,10)
            assert lane["clip"]==committed["clip"] and lane["revision"]==committed["revision"]
            return lane

        def frame(self,name):
            if name in self.frames:return True
            hwnd = self.main_hwnd()
            if name not in self.started_frames:
                geometry.resize_owned_client(hwnd,[680,620]);self.started_frames[name] = time.time();return False
            shot = geometry.copy_owned_product_frame(self.run/"ui.png",self.run/(name+".png"),hwnd,self.started_frames[name])
            if not shot:return False
            self.frames[name] = shot
            return True

        def policy_acks(self):
            path = self.run/"ui.agent.acks.jsonl"
            return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()] if path.exists() else []

    g = Gate()
    committed = None
    done = set()
    seed = authored.clip(authored.event(0,{"type":"text","user":0,"text":"PRIOR"}))
    recorded_actions = [authored.button(0,True),authored.button(0,False),
        {"type":"text","user":0,"text":"REC✓"}]
    saved_clip = g.run/"gui-committed-recording.json"
    prefs = {"version":1,"appearance_mode":"dark","zoom_percent":100,"launch_minimized":False,
        "minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,"shortcuts":[]}

    def no_grant():
        g.reconnect();g.profile_expected = copy.deepcopy(g.inspect()["profile"])
        g.deny_studio("start_without_session_grant",{"operation":"record","user":0,"active":True})

    def partial_scope():
        nonlocal committed
        state = g.reconnect()
        assert state["policy"]["session_control"] and state["policy"]["user_ids"]==ids[:1]
        g.studio({"operation":"load","user":0,"clip":seed})
        committed = copy.deepcopy(g.lane())
        g.focus(0,0)
        g.deny_studio("unpermitted_recording_user",{"operation":"record","user":1,"active":True})
        g.actor_key(True)
        g.native(lambda r:116 in r[0]["held_keys"])
        g.deny_studio("start_preserves_preexisting_actor_hold",{"operation":"record","user":0,"active":True})
        g.actor_key(False);g.native(lambda r:not r[0]["held_keys"])
        g.start_recording()
        before = g.signature()
        denied = g.owner_input({"action":"button","user":0,"button":"back","pressed":True},error=True)
        assert g.signature()==before,"Denied replica route changed working log/native state"
        g.working(0)
        empty = g.studio({"operation":"record","user":0,"active":False},error=True)
        lane = g.prior_preserved()
        assert lane["recording_error"],"Empty discarded take must explain preservation"
        g.record("replica_scope_denial_empty_transaction",{"input_error":denied,"empty_stop_error":empty,
            "preserved":lane,"reports":g.probes()})

    def positive_capture():
        state = g.reconnect()
        assert set(state["policy"]["user_ids"])==set(ids) and state["policy"]["session_control"]
        g.focus(0,0);g.focus(1,1,actor=True)
        g.start_recording()
        for pressed in (True,False):
            g.owner_input({"action":"button","user":0,"button":"back","pressed":pressed})
        g.owner_input({"action":"text","user":0,"text":"REC✓"})
        reports = g.native(lambda r:r[0]["text"]=="REC✓" and not r[0]["held_keys"] and not r[1]["buttons"]
            and [e["pressed"] for e in r[0]["key_events"] if e["vk"]==116][-2:]==[True,False]
            and [e["pressed"] for e in r[1]["mouse_events"] if e["button"]==3]==[True,False])
        assert [e["pressed"] for e in reports[0]["key_events"] if e["vk"]==116][-2:]==[True,False]
        assert [e["pressed"] for e in reports[1]["mouse_events"] if e["button"]==3]==[True,False]
        g.working(3)
        g.actor_input({"Tap":{"device":0xF1000003,"key":116}})
        g.native(lambda r:not r[0]["held_keys"] and len([e for e in r[0]["key_events"] if e["vk"]==116])>=6)
        g.working(3)
        g.deny_studio("foreign_record_stop",{"operation":"record","user":0,"active":False},foreign=True)
        g.deny_studio("foreign_record_replace",{"operation":"load","user":0,"clip":seed},foreign=True)
        g.deny_studio("cancel_does_not_stop_recording",{"operation":"cancel","user":0})
        g.record("logical_source_conversion_single_replica",{"working":g.working(3),"reports":reports,
            "expected_logical_actions":recorded_actions})

    def commit_after_gui_manual():
        nonlocal committed
        g.native(lambda r:len([e for e in r[0]["key_events"] if e["vk"]==118])==2 and 118 not in r[0]["held_keys"])
        before = g.working(3)
        result = g.studio({"operation":"record","user":0,"active":False})
        lane = g.lane()
        candidate = lane["clip"]
        assert not lane["recording"] and lane["recording_client"] is None
        assert candidate["version"]==1 and [e["action"] for e in candidate["events"]]==recorded_actions
        times = [e["at_micros"] for e in candidate["events"]]
        assert times==sorted(times) and all(0<=t<120_000_000 for t in times)
        assert lane["revision"]==committed["revision"]+1
        committed = copy.deepcopy(lane)
        g.record("owner_stop_exact_source_clip_manual_excluded",{"before":before,"stop":result["macro_studio"],"committed":committed})
        g.start_recording()
        g.owner_input({"action":"text","user":0,"text":"EOF✓"})
        g.working(1)
        g.actor_key(True);g.native(lambda r:116 in r[0]["held_keys"])

    def eof_and_prepare_discard():
        g.native(lambda r:87 in r[1]["held_keys"] and 116 in r[0]["held_keys"]
            and [e["pressed"] for e in r[1]["key_events"] if e["vk"]==87]==[True])
        owner = g.lane()["recording_client"]
        g.mcp.close()
        lane = g.prior_preserved()
        reports = g.probes()
        assert 87 in reports[1]["held_keys"] and 116 in reports[0]["held_keys"]
        g.actor_key(False);g.native(lambda r:not r[0]["held_keys"])
        g.record("eof_drops_only_candidate",{"old_owner":owner,"preserved":lane,"reports":reports})
        g.new_owner();g.start_recording()
        g.owner_input({"action":"text","user":0,"text":"DROP✓"})
        g.owner_input({"action":"key","user":0,"key":"f5","pressed":True})
        g.native(lambda r:116 in r[0]["held_keys"])
        g.working(2)

    def discarded():
        lane = g.prior_preserved()
        reports = g.probes()
        assert 116 in reports[0]["held_keys"] and 87 in reports[1]["held_keys"]
        g.owner_input({"action":"key","user":0,"key":"f5","pressed":False})
        g.native(lambda r:not r[0]["held_keys"])
        assert g.prior_preserved()["clip"]==committed["clip"]
        g.record("gui_discard_preserves_direct_input",{"preserved":lane,"reports":reports})

    def saved_and_arm_revoke():
        saved = ui.eventually(lambda:ui.read_json(saved_clip),lambda value:True,3)
        assert saved==committed["clip"]==g.lane()["clip"]
        g.record("gui_cached_committed_recording",{"path":str(saved_clip),
            "sha256":hashlib.sha256(saved_clip.read_bytes()).hexdigest(),"revision":g.lane()["revision"]})
        g.start_recording()
        g.owner_input({"action":"text","user":0,"text":"REVOKE✓"})
        g.working(1)

    def revoked():
        state = g.reconnect()
        assert not state["policy"]["session_control"]
        lane = g.prior_preserved()
        reports = g.probes()
        assert 87 in reports[1]["held_keys"]
        g.deny_studio("revoked_start_denied",{"operation":"record","user":0,"active":True})
        assert ui.read_json(saved_clip)==committed["clip"]
        g.record("revocation_preserves_committed_manual",{"preserved":lane,"reports":reports})
        for client in g.clients:
            if client.process.poll() is None:client.close()
        for client in g.raw_clients:client.close()

    def verify(name,sample):
        if g.profile_expected is not None:
            assert g.snapshot()["profile"]==g.profile_expected
        if name in {"enable","partial","all","revoke"}:
            count = {"enable":1,"partial":2,"all":3,"revoke":4}[name]
            acks = g.policy_acks()
            if len(acks)<count:return False
            assert len(acks)==count and not acks[-1]["pending"] and not acks[-1]["error"]
            assert acks[-1]["policy_enabled"] and not acks[-1]["capture"] and not acks[-1]["app_preferences"]
            assert acks[-1]["session_control"]==(name in {"partial","all"})
            if name not in done:
                {"enable":no_grant,"partial":partial_scope,"all":positive_capture,"revoke":revoked}[name]()
                done.add(name)
            if name=="all" and not g.frame("recording-permissions"):return False
        if name=="manual-tap" and name not in done:
            commit_after_gui_manual();done.add(name)
        if name=="manual-b-hold" and name not in done:
            eof_and_prepare_discard();done.add(name)
        if name=="macros":
            g.working(2)
            if not g.frame("agent-working-versus-committed"):return False
        if name=="discard" and name not in done:
            discarded();done.add(name)
        if name=="save":
            if not saved_clip.exists():return False
            # Capture the idle committed view before arming the next take.
            if not g.frame("committed-after-discard"):return False
            if name not in done:
                saved_and_arm_revoke();done.add(name)
        if name=="stop":
            if any(ui.user32.IsWindow(hwnd) for hwnd in g.layers) or (g.descriptor and g.descriptor.exists()):return False
            reports = g.probes()
            if any(r["held_keys"] or r["buttons"] for r in reports):return False
            edges = [e["pressed"] for e in reports[1]["key_events"] if e["vk"]==87]
            assert edges==[True,False],"GUI B manual hold needs plain native UP on Stop"
            sample["plain_gui_b_w_pair"] = edges
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
        output = {"buttons":[{"type":"legacy"} for _ in range(5)],"keys":{}}
        output["buttons"][3] = {"type":"key","key":116}
        profile = {"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,
            "allowed_pids":g.pids},"replication":{"version":1,"edges":[{"source":ids[0],"target":ids[1],
                "mouse":True,"keyboard":False,"text":False,"options":{"motion":False,"wheel":False,
                "horizontal_wheel":False,"buttons":[False,False,False,True,False]}}]},
            "seats":[{"id":ids[i],"name":"Owned Recording "+("Alpha" if i==0 else "Beta"),
                "mouse":f"virtual://mouse/{i+1}","keyboard":f"virtual://keyboard/{i+1}",
                "desktop_start":g.points[i],"cursor":{"lock_window":False},
                "output":output if i==0 else {"buttons":[{"type":"legacy"} for _ in range(5)],"keys":{}}}
                for i in range(2)]}
        step = ui.step
        steps = [step("start","Start routing"),step("settings","Settings"),
            step("agent-category","Settings category Agent access"),step("agent","Agent Access"),
            step("enable","Enable agent access",{"agent_users":ids[0],"agent_pids":','.join(map(str,g.pids)),
                "agent_session_control":"false","agent_capture":"false","agent_app_preferences":"false"}),
            step("control","Agent session control permission"),step("partial","Apply access policy"),
            step("add-b","Agent user 2"),step("all","Apply access policy"),step("home-lab","Back to Home"),
            step("select-a","Select user 1"),step("settings-lab","Settings"),step("tools-category","Settings category Tools"),
            step("lab","Input lab"),step("manual-tap","Tap key",{"key_name":"f7"}),step("manual-b-hold","2 Hold W"),
            step("home-macro","Back to Home"),step("configure-a","Configure user"),step("macros","Macros"),
            step("discard","Discard agent recording"),step("save","Save user clip JSON",{"studio_file_path":str(saved_clip)}),
            step("home-revoke","Back to Home"),step("settings-revoke","Settings"),
            step("agent-category-revoke","Settings category Agent access"),step("agent-revoke","Agent Access"),
            step("revoke","Apply access policy",{"agent_session_control":"false"}),step("home-stop","Back to Home"),
            step("stop","Stop routing")]
        assert len(steps)==28 and not any({"studio_user","selected_user"}&set(s.get("fields",{})) for s in steps)
        provenance = g.run/"runner-source";provenance.mkdir()
        for name in (Path(__file__).name,"test-connected-macro-studio-acceptance.py",
                     "test-connected-agents-acceptance.py","owned-gui-geometry.py"):
            shutil.copy2(Path(__file__).with_name(name),provenance/name)
        g.evidence["scope"] = "Client-attributed successful logical requests/transactional clip/owner exclusion/conversion+oneBackroute/EOF+Discard+revoke; no faultprovider/fullplayback family/i686 execution"
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
