"""Finite connected VirtualUsers gate. Source-only until explicit frozen READY."""
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
    lifecycle = module("vu_lifecycle", "test-app-lifecycle-ui.py")
    transport = module("vu_transport", "test-connected-agents-acceptance.py")
    actions = module("vu_actions", "test-connected-macro-studio-acceptance.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    ids = ["connected-vu-a", "connected-vu-b"]
    options = {"position":"anchored","offset_x":0,"offset_y":0,"motion":False,
        "wheel":False,"horizontal_wheel":False,"buttons":[False,False,False,True,False],
        "click_delay_ms":0,"step_interval_ms":0}

    class Gate(Base):
        def __init__(self):
            super().__init__(args,"connected-virtual-users",offline=False,count=3)
            self.clients, self.raw_clients = [], []
            self.mcp = self.raw = self.descriptor = None
            self.layers, self.permitted = set(), set()
            self.expected_profile = None
            self.frames, self.frame_started, self.checks = {}, {}, {}

        def probes(self):
            return ui.eventually(lambda:super(Gate,self).probes(),lambda value:True,3)

        def snapshot(self):
            return ui.eventually(lambda:super(Gate,self).snapshot(),lambda value:True,3)

        def fixture(self):
            geometry.physical_pixel_context()
            for i in range(3):
                self.owned.append(subprocess.Popen([str(self.images[0]),"probe","--automation",
                    "--report",str(self.run/f"probe-{i}.json")],creationflags=subprocess.CREATE_NO_WINDOW))
            self.pids = [process.pid for process in self.owned]
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
            found = ui.eventually(windows,lambda value:len(value)==3,20)
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
            assert len(paths)==1,"Discover only own GUI descriptor"
            self.descriptor = paths[0]
            self.new_owner()
            self.raw = transport.OwnedPipe(self.descriptor)
            self.raw_clients.append(self.raw)
            result = self.inspect()
            self.permitted = set(result["policy"]["user_ids"])
            return result

        def automation(self,result,profile_check=True):
            if profile_check and self.expected_profile is not None:
                assert result["profile"]==self.expected_profile,"Unexpected authoritative profile mutation"
            for e in result["events"]:
                if isinstance(e,dict) and "State" in e:
                    self.layers.update(u["overlay"]["hwnd"] for u in e["State"] if u.get("overlay"))
            return [e["Automation"] for e in result["events"] if isinstance(e,dict) and "Automation" in e][-1]

        def inspect(self,check=True):
            result = self.raw.request("inspect")
            self.automation(result,check)
            return result

        def record(self,name,value):
            assert name not in self.checks
            self.checks[name] = value
            self.evidence["checks"] = self.checks
            self.save()

        def actor(self,request):
            sequence = self.raw.sequence
            result = self.raw.request("execute",request=request)
            assert result["client"]==self.raw.client and result["sequence"]==sequence and not result.get("error")
            return result

        def input(self,action):
            return self.mcp.tool("routing_input",action)

        def native(self,predicate):
            return ui.eventually(self.probes,predicate,10)

        def key(self,user,pressed,vk,actor=True):
            before = len([e for e in self.probes()[user]["key_events"] if e["vk"]==vk])
            if actor:
                self.actor({"Input":actions.key(user,pressed,vk,63 if vk==116 else 65)["packet"]})
            else:
                self.input({"action":"key","user":user,"key":"f5" if vk==116 else "f7","pressed":pressed})
            return self.native(lambda r:len([e for e in r[user]["key_events"] if e["vk"]==vk])==before+1
                and ((vk in r[user]["held_keys"])==pressed))

        def focus(self,user,target,actor=False):
            x,y = self.points[target]
            before = len([e for e in self.probes()[target]["mouse_events"] if e["button"]==0])
            if actor:
                self.actor({"MoveTo":{"user":user,"x":x,"y":y}})
                for pressed in (True,False):self.actor({"Input":actions.button(user,pressed,0)["packet"]})
            else:
                self.input({"action":"move_to","user":user,"x":x,"y":y})
                for pressed in (True,False):self.input({"action":"button","user":user,"button":"primary","pressed":pressed})
            self.native(lambda r:len([e for e in r[target]["mouse_events"] if e["button"]==0])==before+2 and not r[target]["buttons"]&1)

        def frame(self,name,size):
            if name in self.frames:return True
            hwnd = self.main_hwnd()
            if name not in self.frame_started:
                geometry.resize_owned_client(hwnd,size);self.frame_started[name] = time.time();return False
            current = geometry.geometry(hwnd)
            if any(abs(a-b)>2 for a,b in zip(current["client_logical"],size)):return False
            shot = geometry.copy_owned_product_frame(self.run/"ui.png",self.run/(name+".png"),hwnd,self.frame_started[name])
            if not shot:return False
            shot["geometry"] = current;self.frames[name] = shot
            return True

        def policy_acks(self):
            path = self.run/"ui.agent.acks.jsonl"
            return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()] if path.exists() else []

        def roster(self):
            result = self.inspect()
            return [e["VirtualUsers"] for e in result["events"] if isinstance(e,dict) and "VirtualUsers" in e][-1]

        def gui_roster(self):
            return ui.eventually(lambda:ui.read_json(self.run/"ui.virtual-users.json"),lambda v:True,3)

        def functional(self):
            result = self.inspect()
            roster = [e["VirtualUsers"] for e in result["events"] if isinstance(e,dict) and "VirtualUsers" in e][-1]
            reports = self.probes()
            return {"profile":result["profile"],"revision":roster["revision"],"slots":roster["slots"],
                "graph_revision":self.automation(result)["graph_revision"],
                "native":[{"keys":r["held_keys"],"buttons":r["buttons"],
                    "key_edges":len(r["key_events"]),"mouse_edges":len(r["mouse_events"])} for r in reports]}

        def deny(self,name,command,request=None,tool="virtual_users"):
            before = self.functional()
            if request is None:reply = self.mcp.tool(tool,command,error=True)
            else:
                sequence = self.raw.sequence
                reply = self.raw.request("execute",request=request)
                assert reply["client"]==self.raw.client and reply["sequence"]==sequence and reply.get("error")
            assert self.functional()==before,"Rejected operation changed roster/graph or native input"
            self.record(name,{"reply":reply,"unchanged":True})

        def project(self,expected):
            actual = self.inspect(check=False)["profile"]
            assert actual==expected,"Authoritative roster projection changed unrelated profile values"
            previous = self.expected_profile
            self.expected_profile = expected
            def gui_profile():
                profile = self.snapshot()["profile"]
                assert profile==previous or profile==expected,"GUI profile suffered unrelated mutation"
                return profile
            ui.eventually(gui_profile,lambda p:p==expected,10)

        def start_clone(self,name):
            before = self.roster()
            result = self.mcp.tool("virtual_users",{"operation":"start","source_id":ids[0],
                "target_pid":self.pids[2],"name":name,"options":options,"expected_revision":before["revision"]})
            state = result["virtual_users"]
            assert state["acknowledged"] and not state.get("error")
            assert state["revision"]==before["revision"]+1 and len(state["slots"])==len(before["slots"])+1
            slot = state["slots"][-1];seat = slot["seat"]
            assert slot["user"]==len(before["slots"]) and slot["active"] and slot["clone_of"]==ids[0]
            assert seat["id"] and all(seat["id"]!=s["seat"]["id"] for s in before["slots"])
            assert seat["mouse"].startswith("virtual://mouse/") and seat["keyboard"].startswith("virtual://keyboard/")
            used = {path for s in before["slots"] for path in (s["seat"]["mouse"],s["seat"]["keyboard"])}
            assert seat["mouse"] not in used and seat["keyboard"] not in used
            expected = copy.deepcopy(self.expected_profile)
            clone = copy.deepcopy(expected["seats"][0])
            clone.update(id=seat["id"],name=name,mouse=seat["mouse"],keyboard=seat["keyboard"],
                target_pid=self.pids[2],desktop_start=None)
            assert seat==clone,"Clone did not copy only the documented source defaults/settings"
            expected["seats"].append(clone)
            expected["replication"]["edges"].append({"source":ids[0],"target":seat["id"],
                "mouse":True,"keyboard":True,"text":True,"options":copy.deepcopy(options)})
            self.project(expected)
            assert self.roster()["slots"]==state["slots"]
            assert seat["id"] not in self.inspect()["policy"]["user_ids"],"Start silently granted its new identity"
            self.record(name,{"ack":state,"graph":expected["replication"]})
            return slot

        def stop_clone(self,slot,name):
            before = self.roster()
            result = self.mcp.tool("virtual_users",{"operation":"stop","id":slot["seat"]["id"],
                "expected_revision":before["revision"]})
            state = result["virtual_users"]
            assert state["acknowledged"] and not state.get("error") and state["revision"]==before["revision"]+1
            assert len(state["slots"])==len(before["slots"]) and not state["slots"][slot["user"]]["active"]
            expected = copy.deepcopy(self.expected_profile)
            expected["replication"]["edges"]=[e for e in expected["replication"]["edges"]
                if slot["seat"]["id"] not in (e["source"],e["target"])]
            self.project(expected)
            self.record(name,{"ack":state,"graph":expected["replication"]})

        def back(self,user,pressed):
            self.input({"action":"button","user":user,"button":"back","pressed":pressed})
            target = user if user<2 else 2
            return self.native(lambda r:bool(r[target]["buttons"]&8)==pressed)

    g = Gate()
    done = set()
    clones = {}
    prefs = {"version":1,"appearance_mode":"dark","zoom_percent":100,"launch_minimized":False,
        "minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,"shortcuts":[]}

    def control_off():
        state = g.reconnect()
        g.expected_profile = copy.deepcopy(state["profile"])
        assert set(state["policy"]["user_ids"])==set(ids) and set(state["policy"]["allowed_pids"])==set(g.pids)
        assert not state["policy"]["session_control"]
        inspected = g.mcp.tool("virtual_users",{"operation":"inspect"})["virtual_users"]
        assert inspected["revision"]==0 and len(inspected["slots"])==2
        g.record("control_off_mcp_inspect",{"state":inspected})
        g.focus(0,0);g.focus(1,1,actor=True)
        g.key(0,True,116)
        g.deny("control_off_start",{"operation":"start","source_id":ids[0],"target_pid":g.pids[2],
            "name":"Denied clone","options":options,"expected_revision":0})

    def creator_group():
        state = g.reconnect()
        assert state["policy"]["session_control"] and set(state["policy"]["user_ids"])==set(ids)
        g.focus(0,0);g.focus(1,1,actor=True)
        g.key(0,True,116,actor=False);g.key(1,True,118)
        command = {"operation":"start","source_id":ids[0],"target_pid":g.pids[2],"name":"Denied clone","options":options}
        g.deny("roster_revision_required",command)
        g.deny("stale_roster_revision",dict(command,expected_revision=17))
        g.deny("original_stop_rejected",{"operation":"stop","id":ids[0],"expected_revision":0})
        clones["c"] = g.start_clone("Owned transient C")
        assert clones["c"]["user"]==2
        g.deny("ungranted_direct_input",{"action":"button","user":2,"button":"back","pressed":True},tool="routing_input")
        g.deny("ungranted_source_fanout",{"action":"button","user":0,"button":"back","pressed":True},tool="routing_input")
        g.deny("foreign_ungranted_stop",{},request={"VirtualUsers":{"operation":"stop",
            "id":clones["c"]["seat"]["id"],"expected_revision":1}})
        g.stop_clone(clones["c"],"creator_aborts_own_ungranted")
        g.native(lambda r:116 in r[0]["held_keys"] and 118 in r[1]["held_keys"])
        clones["d"] = g.start_clone("Owned grant D")
        assert clones["d"]["user"]==3
        g.native(lambda r:116 in r[0]["held_keys"] and 118 in r[1]["held_keys"])
        g.record("initial_holds_survive_lifecycle",{"reports":g.probes()})

    def granted_and_eof():
        state = g.reconnect()
        assert set(state["policy"]["user_ids"])==set(ids+[clones["d"]["seat"]["id"]])
        assert set(state["policy"]["allowed_pids"])==set(g.pids)
        assert g.roster()["slots"][3]["active"]
        g.focus(0,0);g.focus(1,1,actor=True)
        before = [len([e for e in r["mouse_events"] if e["button"]==3]) for r in g.probes()]
        g.back(0,True);g.back(0,False)
        reports = g.native(lambda r:all(len([e for e in r[i]["mouse_events"] if e["button"]==3])==before[i]+2
            for i in (0,2)) and not r[0]["buttons"] and not r[2]["buttons"])
        for i in (0,2):assert [e["pressed"] for e in reports[i]["mouse_events"] if e["button"]==3][-2:]==[True,False]
        g.record("explicit_gui_grant_enables_native_copy",{"reports":reports,"policy":state["policy"]})
        g.key(1,True,118)
        g.back(3,True)
        g.stop_clone(clones["d"],"granted_stop")
        reports = g.native(lambda r:not r[2]["buttons"] and 118 in r[1]["held_keys"])
        g.record("granted_stop_preserves_unrelated_owner",{"reports":reports})
        g.deny("stopped_slot_input",{"action":"button","user":3,"button":"back","pressed":True},tool="routing_input")
        clones["e"] = g.start_clone("Owned EOF E")
        assert clones["e"]["user"]==4
        before = g.functional()
        g.mcp.close()
        ui.eventually(lambda:g.inspect()["clients"],lambda clients:g.raw.client in clients and len(clients)==1,10)
        assert g.functional()==before,"EOF removed GUI-owned clone/graph or another actor's input"
        sequence = g.raw.sequence
        reply = g.raw.request("execute",request={"VirtualUsers":{"operation":"stop",
            "id":clones["e"]["seat"]["id"],"expected_revision":5}})
        assert reply["client"]==g.raw.client and reply["sequence"]==sequence and reply.get("error")
        assert g.functional()==before
        g.record("eof_preserves_clone_no_inherited_abort",{"reply":reply,"state":before})

    def verify(name,sample):
        if g.expected_profile is not None and name not in {"local-stop","stop"}:
            assert g.snapshot()["profile"]==g.expected_profile
        if name in {"enable","full","grant"}:
            count = {"enable":1,"full":2,"grant":3}[name]
            acks = g.policy_acks()
            if len(acks)<count:return False
            assert len(acks)==count and not acks[-1]["pending"] and not acks[-1]["error"]
            assert not acks[-1]["capture"] and not acks[-1]["app_preferences"]
            if name not in done:
                {"enable":control_off,"full":creator_group,"grant":granted_and_eof}[name]()
                done.add(name)
            if name=="full" and not g.frame("ungranted-generated-user",[680,620]):return False
        if name in {"replicas","select-e"}:
            actual = g.gui_roster()
            if not actual["state"] or actual["state"]["revision"]!=5:return False
            assert actual["state"]["slots"]==g.roster()["slots"] and not actual["pending"]
            if name=="select-e":
                assert actual["selected_clone_id"]==clones["e"]["seat"]["id"]
                if not g.frame("eof-gui-owned-replica",[760,620]):return False
        if name=="local-stop":
            actual = g.gui_roster()
            if not actual["state"] or actual["state"]["revision"]!=6:return False
            assert not actual["state"]["slots"][4]["active"] and not actual["pending"]
            if name not in done:
                expected = copy.deepcopy(g.expected_profile);expected["replication"]["edges"]=[]
                g.project(expected)
                reports = g.native(lambda r:not r[2]["buttons"] and 118 in r[1]["held_keys"])
                g.record("local_gui_stop_preserves_other_actor",{"gui":actual,"reports":reports})
                g.key(1,False,118);done.add(name)
            if not g.frame("gui-stopped-tombstones",[760,620]):return False
        if name=="stop":
            expected = copy.deepcopy(g.expected_profile)
            expected["seats"] = expected["seats"][:2]
            expected["replication"]["edges"] = []
            actual = g.snapshot()["profile"]
            assert actual==g.expected_profile or actual==expected,"Stop changed original profile settings"
            if actual!=expected:return False
            g.expected_profile = expected
            if any(ui.user32.IsWindow(hwnd) for hwnd in g.layers) or (g.descriptor and g.descriptor.exists()):return False
            if any(r["held_keys"] or r["buttons"] for r in g.probes()):return False
            for client in g.clients:
                if client.process.poll() is None:client.close()
            for client in g.raw_clients:client.close()
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
        profile = {"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,
            "allowed_pids":g.pids},"replication":{"version":1,"edges":[]},"seats":[{"id":ids[i],
                "name":"Owned VU "+("Alpha" if i==0 else "Beta"),"mouse":f"virtual://mouse/{i+1}",
                "keyboard":f"virtual://keyboard/{i+1}","desktop_start":g.points[i],"cursor":{"lock_window":False}}
                for i in range(2)]}
        step = ui.step
        steps = [step("start","Start routing"),step("settings","Settings"),
            step("agent-category","Settings category Agent access"),step("agent","Agent Access"),
            step("enable","Enable agent access",{"agent_users":','.join(ids),"agent_pids":','.join(map(str,g.pids)),
                "agent_session_control":"false","agent_capture":"false","agent_app_preferences":"false"}),
            step("control","Agent session control permission"),step("full","Apply access policy"),
            step("grant-d","Agent user 4"),step("grant","Apply access policy"),
            step("home-replicas","Back to Home"),step("settings-replicas","Settings"),
            step("automation-category","Settings category Automation"),step("replicas","Virtual Replicas"),
            step("select-e","Select replica 5"),step("local-stop","Stop selected virtual replica"),
            step("select-stopped-e","Select replica 5"),step("home-stop","Back to Home"),step("stop","Stop routing")]
        assert len(steps)==18
        provenance = g.run/"runner-source";provenance.mkdir()
        for name in (Path(__file__).name,"test-connected-macro-studio-acceptance.py",
                "test-connected-agents-acceptance.py","owned-gui-geometry.py"):
            shutil.copy2(Path(__file__).with_name(name),provenance/name)
        g.evidence["scope"] = "Connected VirtualUsers authority/ungranted creation receipt/grant+reconnect/native Back copy/Stop+EOF GUI ownership only;3ownx64targets,no i686 execution"
        g.execute(profile,prefs,steps,verify,lambda gate:gate.main_hwnd() is not None,after)
        g.evidence.update(result="PENDING INDEPENDENT VISUAL REVIEW",control_state_result="PASS",captures=g.frames)
        g.save();return g.run
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
    for name in ("executable","x86-executable","runtime"):parser.add_argument("--"+name,type=Path,required=True)
    args = parser.parse_args()
    assert args.ready,"Await frozen READY/exclusive native slot"
    workflow(args)


if __name__=="__main__":main()
