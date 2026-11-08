"""Focused missing replica/Macro visual correction; frozen READY required."""
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
    lifecycle = module("utility_lifecycle", "test-app-lifecycle-ui.py")
    transport = module("utility_transport", "test-connected-agents-acceptance.py")
    actions = module("utility_actions", "test-connected-macro-studio-acceptance.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    ids = ["utility-a", "utility-b"]
    options = {"position":"anchored","offset_x":0,"offset_y":0,"motion":False,
        "wheel":False,"horizontal_wheel":False,"buttons":[False,False,False,True,False],
        "click_delay_ms":0,"step_interval_ms":0}
    graph = {"version":1,"edges":[{"source":ids[0],"target":ids[1],"mouse":True,
        "keyboard":False,"text":False,"options":copy.deepcopy(options)}]}
    if args.remaining_tail:
        assert args.from_run
        prefix = ui.read_json(args.from_run/"evidence.json")
        assert prefix["result"]=="PENDING INDEPENDENT VISUAL REVIEW" and prefix["control_state_result"]=="PASS"
        assert len(prefix["samples"])==17
        actual_events = [json.loads(line) for line in (args.from_run/"ui.events.jsonl").read_text(encoding="utf-8").splitlines()]
        assert len(actual_events)==17 and actual_events[-1]["button"]=="Discard agent recording"
        assert prefix["owned_processes_closed"]
        assert prefix["samples"]["enable"]["profile_state"]["profile"]["replication"]==graph
        assert ui.read_json(args.from_run/"independent-visual-review.json")["result"]=="NOT ACCEPTED"

    class Gate(Base):
        def __init__(self):
            super().__init__(args,"productivity-ux",offline=False,count=3)
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

        def graph(self):
            return ui.eventually(lambda:ui.read_json(self.run/"ui.graph.json"),lambda v:True,3)

        def lane(self):
            state = self.mcp.tool("macro_studio",{"operation":"inspect","user":0})["macro_studio"]
            return next(user for user in state["users"] if user["user"]==0)

        def revealed_frame(self,name,next_label):
            # Resize first, then let the normal next-control observer reveal
            # its real header/action. This does not synthesize wheel input.
            if name not in self.frame_started:return self.frame(name,[320,360])
            progress = ui.read_json(self.run/"ui.progress.json")
            if progress.get("response_label")!=next_label or not progress.get("response_visible"):return False
            if time.time()-self.frame_started[name]<.5:return False
            return self.frame(name,[320,360])

    g = Gate()
    done = set()
    values = {}
    prefs = {"version":1,"appearance_mode":"dark","zoom_percent":100,"launch_minimized":False,
        "minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,"shortcuts":[]}

    def verify(name,sample):
        if g.expected_profile is not None and name not in {"apply-graph","start-clone","stop-clone"}:
            assert g.snapshot()["profile"]==g.expected_profile
        if name=="enable":
            if not g.policy_acks():return False
            if name not in done:
                state = g.reconnect()
                assert set(state["policy"]["user_ids"])==set(ids)
                assert set(state["policy"]["allowed_pids"])==set(g.pids) and state["policy"]["session_control"]
                assert not state["policy"]["capture"] and not state["policy"]["app_preferences"]
                g.expected_profile = copy.deepcopy(state["profile"])
                values["initial_profile"] = copy.deepcopy(g.expected_profile)
                values["preferences_bytes"] = g.preferences_path.read_bytes()
                g.focus(0,0);g.focus(1,1,actor=True)
                assert not any(e["button"]==0 for e in g.probes()[2]["mouse_events"])
                done.add(name)
        if name=="connect":
            current = g.graph()
            if current["draft"]!=graph:return False
            assert current["applied"]=={"version":1,"edges":[]} and not current["pending"]
            if not g.frame("graph-staged-normal",[860,680]):return False
        if name=="apply-graph":
            current = g.graph()
            if current["applied"]!=graph or current["pending"]:return False
            assert current["graph_revision"]==1 and not current["error"]
            if name not in done:
                expected = copy.deepcopy(g.expected_profile);expected["replication"] = copy.deepcopy(graph)
                g.project(expected)
                values["graph_applied"] = current
                done.add(name)
        if name=="duplicate":
            current = g.graph()
            if not current["error"]:return False
            assert current["draft"]==graph and current["applied"]==graph and not current["pending"]
            if name not in done:
                before_error = current["error"]
                before = g.probes()[2]
                g.focus(0,2)
                delivered = g.probes()[2]
                assert len([e for e in delivered["mouse_events"] if e["button"]==0])==len([e for e in before["mouse_events"] if e["button"]==0])+2
                status = ui.eventually(g.snapshot,lambda s:f"Preparing application {g.pids[2]}" not in s["status"],10)
                # A later local command must not mask attachment completion.
                assert g.graph()["error"]==before_error and g.graph()["draft"]==graph
                g.record("connected_new_target_completion_preserves_page_error",{
                    "target_pid":g.pids[2],"native_before":before,"native_after":delivered,
                    "displayed_status":status["status"],"graph_error":before_error})
                g.focus(0,0)
                assert not g.probes()[2]["buttons"],"Release reserved target before clone attachment"
                done.add(name)
            assert g.graph()["error"]==current["error"]
            if not g.frame("graph-error-after-attach-minimum",[320,360]):return False
        if name=="start-clone":
            current = g.gui_roster()
            if not current["state"] or current["state"]["revision"]!=1 or current["pending"]:return False
            if name not in done:
                slot = current["state"]["slots"][-1]
                assert slot["user"]==2 and slot["active"] and slot["clone_of"]==ids[0]
                seat = slot["seat"]
                expected = copy.deepcopy(g.expected_profile)
                clone = copy.deepcopy(expected["seats"][0])
                clone.update(id=seat["id"],name="Owned utility replica",mouse=seat["mouse"],
                    keyboard=seat["keyboard"],target_pid=g.pids[2],desktop_start=None)
                assert seat==clone and seat["id"] not in ids
                expected["seats"].append(clone)
                expected["replication"]["edges"].append({"source":ids[0],"target":seat["id"],
                    "mouse":True,"keyboard":True,"text":True,"options":copy.deepcopy(options)})
                g.project(expected)
                values["clone"] = slot
                g.record("local_clone_ack",{"state":current["state"]})
                done.add(name)
        if name=="select-clone":
            current = g.gui_roster()
            if current["selected_clone_id"]!=values["clone"]["seat"]["id"]:return False
            assert not current["pending"] and current["state"]["slots"][2]["active"]
        if name=="stop-clone":
            current = g.gui_roster()
            if not current["state"] or current["state"]["revision"]!=2 or current["pending"]:return False
            assert not current["state"]["slots"][2]["active"]
            if name not in done:
                expected = copy.deepcopy(g.expected_profile);expected["replication"] = copy.deepcopy(graph)
                g.project(expected);done.add(name)
            if not g.revealed_frame("replica-selected-tombstone-minimum","Stable identity details"):return False
        if name=="replica-details":
            current = g.gui_roster()
            assert current["selected_clone_id"]==values["clone"]["seat"]["id"]
            assert current["state"]["revision"]==2 and not current["state"]["slots"][2]["active"] and not current["pending"]
            if not g.revealed_frame("replica-identities-bottom-minimum","Stop selected virtual replica"):return False
        if name=="stopped-action":
            current = g.gui_roster()
            assert current["state"]["revision"]==2 and not current["state"]["slots"][2]["active"]
            assert not current["pending"] and sample["event"]["observed_disabled"] and not sample["event"]["clicked"]
        if name=="macro":
            if name not in done:
                lane = g.lane()
                assert not lane["recording"] and lane["loaded_events"]==0 and not lane.get("playback")
                if not g.frame("macro-idle-normal",[760,620]):return False
                state = g.mcp.tool("macro_studio",{"operation":"record","user":0,"active":True})["macro_studio"]
                assert state["acknowledged"] and not state.get("error")
                values["recording_client"] = g.lane()["recording_client"]
                assert values["recording_client"]
                done.add(name)
            lane = g.lane()
            assert lane["recording"] and lane["recording_client"]==values["recording_client"] and lane["recorded_events"]==0
            if not g.frame("macro-agent-active-minimum",[320,360]):return False
        if name=="discard":
            lane = g.lane()
            if lane["recording"]:return False
            assert lane["loaded_events"]==0 and lane["recorded_events"]==0
            if name not in done:
                g.record("real_gui_discard_empty_agent_candidate",{"lane":lane})
                done.add(name)
        return True

    def after(gate):
        assert gate.snapshot()["profile"]==gate.expected_profile
        assert gate.preferences_path.read_bytes()==values["preferences_bytes"]
        assert not any(r["held_keys"] or r["buttons"] for r in gate.probes())
        ui.put(gate.run/"final-profile-before-close.json",gate.snapshot()["profile"])
        gate.expected_exit = True
        assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0)
        gate.gui.wait(timeout=15)
        assert gate.gui.returncode==0 and not hotkeys.lifecycle_window(gate.gui.pid)
        assert all(not ui.user32.IsWindow(hwnd) for hwnd in gate.layers)
        assert not gate.descriptor.exists()
        return True

    try:
        g.fixture()
        profile = {"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,
            "allowed_pids":g.pids},"replication":{"version":1,"edges":[]},"seats":[{"id":ids[i],
                "name":"Owned utility "+("Alpha" if i==0 else "Beta"),"mouse":f"virtual://mouse/{i+1}",
                "keyboard":f"virtual://keyboard/{i+1}","desktop_start":g.points[i],"cursor":{"lock_window":False}}
                for i in range(2)]}
        graph_fields = {"graph_source":ids[0],"graph_target":ids[1],"graph_mouse":"true",
            "graph_keyboard":"false","graph_text":"false","graph_motion":"false","graph_wheel":"false",
            "graph_horizontal_wheel":"false","graph_button_primary":"false","graph_button_secondary":"false",
            "graph_button_middle":"false","graph_button_back":"true","graph_button_forward":"false"}
        clone_fields = {"clone_source_id":ids[0],"clone_target_pid":str(g.pids[2]),"clone_name":"Owned utility replica",
            "clone_motion":"false","clone_wheel":"false","clone_horizontal_wheel":"false",
            "clone_button_primary":"false","clone_button_secondary":"false","clone_button_middle":"false",
            "clone_button_back":"true","clone_button_forward":"false"}
        step = ui.step
        steps = [step("start","Start routing"),step("settings","Settings"),
            step("agent-category","Settings category Agent access"),step("agent","Agent Access"),
            step("enable","Enable agent access",{"agent_users":','.join(ids),"agent_pids":','.join(map(str,g.pids)),
                "agent_session_control":"true","agent_capture":"false","agent_app_preferences":"false"}),
            step("home-graph","Back to Home"),step("settings-graph","Settings"),
            step("automation-category","Settings category Automation"),step("graph","Replication Graph"),
            step("connection-details","Connect users"),step("connect","Connect selected users",graph_fields),
            step("apply-graph","Apply replication graph"),step("duplicate","Connect selected users"),
            step("home-replicas","Back to Home"),step("settings-replicas","Settings"),step("replicas","Virtual Replicas"),
            step("start-clone","Start virtual replica",clone_fields),step("select-clone","Select replica 3"),
            step("stop-clone","Stop selected virtual replica"),step("replica-details","Stable identity details"),
            step("home-macro","Back to Home"),step("settings-macro","Settings"),step("macro","Macro Studio"),
            step("discard","Discard agent recording")]
        assert len(steps)==24
        if args.remaining_tail:
            # Explicit initial fixture, not a replacement proof for prior live Apply.
            profile["replication"] = copy.deepcopy(graph)
            g.evidence["retained_prefix"]={"path":str(args.from_run/"evidence.json"),
                "sha256":hashlib.sha256((args.from_run/"evidence.json").read_bytes()).hexdigest(),
                "result":prefix["result"],"accepted_controls":17,"actual_receipts":17,
                "reason":"Only missing minimum reveal and corrected Macro pixels; prior functional results and normal VU image retained. Fresh C is never clicked before VUStart."}
            steps = steps[:8]+steps[15:]
            assert len(steps)==17
            steps.insert(13,step("stopped-action","Stop selected virtual replica",disabled=True))
            assert len(steps)==18
        provenance = g.run/"runner-source";provenance.mkdir()
        for name in (Path(__file__).name,"test-connected-macro-studio-acceptance.py",
                "test-connected-agents-acceptance.py","owned-gui-geometry.py"):
            shutil.copy2(Path(__file__).with_name(name),provenance/name)
        g.evidence["scope"] = "Focused replica minimum reveal and corrected Macro idle/agent pixels only; 17clicks+1disabled observation, Graph/status and normal active VU retained from prior artifacts"
        g.execute(profile,prefs,steps,verify,lambda gate:gate.main_hwnd() is not None,after)
        assert len(g.frames)==(4 if args.remaining_tail else 6)
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
    parser.add_argument("--remaining-tail",action="store_true")
    parser.add_argument("--from-run",type=Path)
    for name in ("executable","x86-executable","runtime"):parser.add_argument("--"+name,type=Path,required=True)
    args = parser.parse_args()
    assert args.ready and args.remaining_tail and args.from_run,"Await frozen correction READY; exact prior raw run required"
    workflow(args)


if __name__=="__main__":main()
