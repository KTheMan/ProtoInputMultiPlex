"""Finite connected graph transactions gate; no apps before explicit frozen READY."""
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


def definition(source, target, delay=5000):
    return {"version":1,"edges":[{"source":source,"target":target,"mouse":True,
        "keyboard":False,"text":False,"options":{"position":"anchored","offset_x":0,"offset_y":0,
            "motion":False,"wheel":False,"horizontal_wheel":False,
            "buttons":[False,False,False,True,False],"click_delay_ms":delay,"step_interval_ms":0}}]}


def workflow(args):
    lifecycle = module("connected_graph_lifecycle","test-app-lifecycle-ui.py")
    transport = module("connected_graph_transport","test-connected-agents-acceptance.py")
    actions = module("connected_graph_actions","test-connected-macro-studio-acceptance.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    ids = ["connected-graph-a","connected-graph-b"]
    empty = {"version":1,"edges":[]}
    graph_one, graph_two = definition(*ids), definition(*ids,delay=1000)
    if args.save_tail:
        assert args.from_run
        prefix = ui.read_json(args.from_run/"evidence.json")
        assert prefix["result"]=="NOT ACCEPTED" and len(prefix["samples"])==21
        assert prefix["checks"]["external_ack_preserves_unsent_draft"]["gui_graph"]["applied"]==graph_two
        assert prefix["samples"]["stop"] and prefix["owned_processes_closed"]

    class Gate(Base):
        def __init__(self):
            super().__init__(args,"connected-graph",offline=False,count=2)
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
            for i in range(2):
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

        def graph_state(self):
            result = self.inspect()
            state = self.automation(result)
            return {"revision":state["graph_revision"],"definition":result["profile"]["replication"]}

        def lane(self):
            result = self.raw.request("execute",request={"MacroStudio":{"operation":"inspect","user":0}})
            assert not result.get("error")
            self.automation(result)
            return [e["MacroStudio"] for e in result["events"] if isinstance(e,dict) and "MacroStudio" in e][-1]["users"][0]

        def signature(self):
            result = self.inspect()
            state = self.automation(result)
            lane = self.lane()
            reports = self.probes()
            queue = state["replication"]
            # A due copy may legitimately transition pending→emitted while a
            # rejected request is checked. Conservation/cancel counters plus
            # ordinary owners are stable; native replica timing is not frozen.
            return {"profile":result["profile"],"revision":state["graph_revision"],
                "recorder":{k:lane.get(k) for k in ("recording","recording_client","recorded_events","clip","revision")},
                "ordinary":{"a_keys":reports[0]["held_keys"],"a_buttons":reports[0]["buttons"],
                    "b_keys":reports[1]["held_keys"]},
                "queue":{"accounted":queue["pending_events"]+queue["emitted"],
                    "cancelled":queue["cancelled"],"suspended":queue["suspended"]}}

        def record(self,name,value):
            assert name not in self.checks
            self.checks[name] = value
            self.evidence["checks"] = self.checks
            self.save()

        def deny(self,name,command,raw_request=None):
            before = self.signature()
            if raw_request is None:
                result = self.mcp.tool("routing_graph",command,error=True)
            else:
                sequence = self.raw.sequence
                result = self.raw.request("execute",request=raw_request)
                assert result["client"]==self.raw.client and result["sequence"]==sequence and result.get("error")
            assert self.signature()==before,"Rejected graph command changed graph/program/ordinary owners or cancelled work"
            self.record(name,{"reply":result,"unchanged":True})

        def apply(self,graph,revision):
            result = self.mcp.tool("routing_graph",{"operation":"apply","graph":graph,"expected_revision":revision})
            assert result["automation"]["acknowledged"] and not result["automation"].get("error")
            assert result["graph"]=={"revision":revision+1,"definition":graph}
            expected = copy.deepcopy(self.expected_profile)
            expected["replication"] = copy.deepcopy(graph)
            snapshot = self.inspect(check=False)
            assert snapshot["profile"]==expected,"Apply changed unrelated settings or returned stale profile"
            self.expected_profile = expected
            self.automation(snapshot)
            return result

        def cancel(self,revision):
            before = self.graph_state()
            result = self.mcp.tool("routing_graph",{"operation":"cancel","expected_revision":revision})
            assert result["automation"]["acknowledged"] and not result["automation"].get("error")
            assert result["graph"]==before==self.graph_state()
            return result

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

        def back(self,pressed,actor=False):
            if actor:self.actor({"Input":actions.button(0,pressed)["packet"]})
            else:self.input({"action":"button","user":0,"button":"back","pressed":pressed})
            return self.native(lambda r:bool(r[0]["buttons"]&8)==pressed)

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

        def gui_graph(self):
            return ui.eventually(lambda:ui.read_json(self.run/"ui.graph.json"),lambda value:True,3)

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

    g = Gate()
    done = set()
    saved_profile = g.run/"committed-connected-graph.json"
    prefs = {"version":1,"appearance_mode":"dark","zoom_percent":100,"launch_minimized":False,
        "minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,"shortcuts":[]}

    def permission_stage(name):
        state = g.reconnect()
        if g.expected_profile is None:g.expected_profile = copy.deepcopy(state["profile"])
        g.focus(0,0)
        g.key(0,True,116)
        g.deny(name+"_apply_denied",{"operation":"apply","graph":graph_one,"expected_revision":0})
        g.deny(name+"_cancel_denied",{"operation":"cancel","expected_revision":0})

    def full_scope():
        state = g.reconnect()
        assert set(state["policy"]["user_ids"])==set(ids) and set(state["policy"]["allowed_pids"])==set(g.pids)
        assert state["policy"]["session_control"]
        g.focus(0,0);g.focus(1,1,actor=True)
        g.key(0,True,116,actor=False);g.key(1,True,118)
        g.deny("explicit_revision_required",{"operation":"apply","graph":graph_one})
        g.deny("numeric_raw_bypass_denied",{},raw_request={"Automation":{"Replication":{"edges":[]}}})
        result = g.apply(graph_one,0)
        reports = g.native(lambda r:not any(x["held_keys"] or x["buttons"] for x in r)
            and [e["pressed"] for e in r[0]["key_events"] if e["vk"]==116][-2:]==[True,False]
            and [e["pressed"] for e in r[1]["key_events"] if e["vk"]==118][-2:]==[True,False])
        g.record("apply_global_plain_release",{"ack":result["automation"],"graph":result["graph"],"reports":reports})
        # Re-establish actual child focus after the intentional global reset.
        g.focus(0,0);g.focus(1,1,actor=True)
        g.mcp.tool("macro_studio",{"operation":"record","user":0,"active":True})
        g.back(True,actor=True);g.key(1,True,118)
        ui.eventually(lambda:g.automation(g.inspect())["replication"],lambda q:q["pending_events"]>0,5)
        g.deny("stale_revision_preserves_busy_state",{"operation":"apply","graph":graph_one,"expected_revision":0})
        cycle = copy.deepcopy(graph_one);cycle["edges"] += definition(ids[1],ids[0])["edges"]
        g.deny("cycle_preserves_busy_state",{"operation":"apply","graph":cycle,"expected_revision":1})
        g.deny("recording_apply_preserves_busy_state",{"operation":"apply","graph":graph_one,"expected_revision":1})
        g.native(lambda r:r[1]["buttons"]&8)
        g.back(False,actor=True);g.key(0,True,116)
        before = ui.eventually(lambda:g.automation(g.inspect())["replication"],lambda q:q["pending_events"]>0,5)
        result = g.cancel(1)
        reports = g.native(lambda r:not r[1]["buttons"] and 116 in r[0]["held_keys"] and 118 in r[1]["held_keys"])
        lane = g.lane();after = g.automation(g.inspect())["replication"]
        assert lane["recording"] and lane["recorded_events"]==0
        assert after["pending_events"]==0 and after["cancelled"]>before["cancelled"]
        g.record("cancel_preserves_recording_and_ordinary_owners",{"ack":result["automation"],"before":before,
            "after":after,"lane":lane,"reports":reports})
        g.mcp.tool("macro_studio",{"operation":"record","user":0,"active":False},error=True)
        g.key(0,False,116);g.key(1,False,118)
        before_edges = len([e for e in g.probes()[1]["mouse_events"] if e["button"]==3])
        g.back(True);g.back(False)
        reports = g.native(lambda r:len([e for e in r[1]["mouse_events"] if e["button"]==3])==before_edges+2
            and not r[1]["buttons"])
        assert [e["pressed"] for e in reports[1]["mouse_events"] if e["button"]==3][-2:]==[True,False]
        g.record("future_events_after_cancel",{"reports":reports,"graph":g.graph_state()})
        g.back(True);g.key(1,True,118)
        pending = ui.eventually(lambda:g.automation(g.inspect())["replication"],lambda q:q["pending_events"]>0,5)
        graph_before = g.graph_state()
        g.mcp.close()
        reports = g.native(lambda r:not r[0]["buttons"] and 118 in r[1]["held_keys"])
        after = ui.eventually(lambda:g.automation(g.inspect())["replication"],lambda q:q["pending_events"]==0,10)
        assert after["cancelled"]>pending["cancelled"] and g.graph_state()==graph_before
        g.key(1,False,118)
        g.new_owner()
        g.record("owner_eof_cleans_only_its_queue_preserves_graph",{"before":pending,"after":after,"reports":reports,
            "graph":graph_before})

    def apply_while_draft_dirty():
        actual = g.gui_graph()
        assert actual["draft"]==empty and actual["applied"]==graph_one and not actual["pending"]
        result = g.apply(graph_two,1)
        actual = ui.eventually(g.gui_graph,lambda s:s["applied"]==graph_two and s["graph_revision"]==2,10)
        assert actual["draft"]==empty and not actual["pending"]
        g.record("external_ack_preserves_unsent_draft",{"ack":result["automation"],"gui_graph":actual})

    def verify(name,sample):
        if g.expected_profile is not None:
            assert g.snapshot()["profile"]==g.expected_profile
        if name=="tail-enable":
            if len(g.policy_acks())<1:return False
            if name not in done:
                state = g.reconnect()
                assert set(state["policy"]["user_ids"])==set(ids)
                assert set(state["policy"]["allowed_pids"])==set(g.pids) and state["policy"]["session_control"]
                g.expected_profile = copy.deepcopy(state["profile"])
                g.focus(0,0);g.focus(1,1,actor=True)
                result = g.apply(graph_two,0)
                g.record("fresh_ack_to_gui_profile",{"ack":result["automation"],"graph":result["graph"]})
                done.add(name)
        if name in {"enable","partial-users","partial-pids","full"}:
            count = {"enable":1,"partial-users":2,"partial-pids":3,"full":4}[name]
            acks = g.policy_acks()
            if len(acks)<count:return False
            ack = acks[-1]
            assert len(acks)==count and not ack["pending"] and not ack["error"] and ack["policy_enabled"]
            assert not ack["capture"] and not ack["app_preferences"] and ack["session_control"]==(name!="enable")
            if name not in done:
                if name=="full":full_scope()
                else:permission_stage(name)
                done.add(name)
            if name=="full" and not g.frame("whole-session-graph-grant",[680,620]):return False
        if name in {"graph","node-before"}:
            graph = g.gui_graph()
            if graph["applied"]!=graph_one or graph["graph_revision"]!=1:return False
            assert graph["draft"]==graph_one and not graph["pending"]
            sample["graph"] = graph
            if name=="graph" and not g.frame("connected-applied-graph",[900,720]):return False
        if name=="clear-draft":
            graph = g.gui_graph()
            if graph["draft"]!=empty:return False
            if name not in done:
                apply_while_draft_dirty();done.add(name)
            graph = g.gui_graph()
            assert graph["draft"]==empty and graph["applied"]==graph_two and not graph["pending"]
            sample["graph"] = graph
            if not g.frame("external-applied-unsent-draft",[900,720]):return False
        if name=="node-after":
            graph = g.gui_graph()
            assert graph["draft"]==empty and graph["applied"]==graph_two
            sample["graph"] = graph
        if name=="stop":
            if any(ui.user32.IsWindow(hwnd) for hwnd in g.layers) or (g.descriptor and g.descriptor.exists()):return False
            if any(r["held_keys"] or r["buttons"] for r in g.probes()):return False
            for client in g.clients:
                if client.process.poll() is None:client.close()
            for client in g.raw_clients:client.close()
        if name=="save-profile":
            if not saved_profile.exists():return False
            saved = ui.eventually(lambda:ui.read_json(saved_profile),lambda value:True,3)
            assert saved==g.expected_profile,"Actual GUI Save persisted stale/draft graph instead of ACK profile"
            g.record("gui_profile_save_authoritative_graph",{"path":str(saved_profile),
                "sha256":hashlib.sha256(saved_profile.read_bytes()).hexdigest(),"graph":saved["replication"]})
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
            "allowed_pids":g.pids},"replication":empty,"seats":[{"id":ids[i],
                "name":"Owned Graph "+("Alpha" if i==0 else "Beta"),"mouse":f"virtual://mouse/{i+1}",
                "keyboard":f"virtual://keyboard/{i+1}","desktop_start":g.points[i],"cursor":{"lock_window":False}}
                for i in range(2)]}
        step = ui.step
        steps = [step("start","Start routing"),step("settings","Settings"),
            step("agent-category","Settings category Agent access"),step("agent","Agent Access"),
            step("enable","Enable agent access",{"agent_users":ids[0],"agent_pids":str(g.pids[0]),
                "agent_session_control":"false","agent_capture":"false","agent_app_preferences":"false"}),
            step("control","Agent session control permission"),
            step("partial-users","Apply access policy",{"agent_pids":','.join(map(str,g.pids))}),
            step("add-b","Agent user 2"),step("partial-pids","Apply access policy",{"agent_pids":str(g.pids[0])}),
            step("full","Apply access policy",{"agent_pids":','.join(map(str,g.pids))}),
            step("home-graph","Back to Home"),step("settings-graph","Settings"),
            step("automation-category","Settings category Automation"),step("graph","Replication Graph"),
            step("node-before","Graph user 1"),step("clear-draft","Clear replication graph"),
            step("node-after","Graph user 1"),step("home-stop","Back to Home"),step("stop","Stop routing"),
            step("settings-save","Settings"),step("application-category","Settings category Application"),
            step("profiles","Profiles & backend"),step("save-profile","Save profile",{"profile_path":str(saved_profile)}),
            step("home-final","Back to Home")]
        assert len(steps)==24
        if args.save_tail:
            assert list(g.evidence["build_sha256"].values())==list(prefix["build_sha256"].values()),"Continuation must use identical frozen images"
            g.evidence["retained_prefix"]={"path":str(args.from_run/"evidence.json"),
                "sha256":hashlib.sha256((args.from_run/"evidence.json").read_bytes()).hexdigest(),
                "result":prefix["result"],"accepted_controls":21,"native_subchecks":len(prefix["checks"])}
            steps = [step("start","Start routing"),step("settings","Settings"),
                step("agent-category","Settings category Agent access"),step("agent","Agent Access"),
                step("tail-enable","Enable agent access",{"agent_users":','.join(ids),
                    "agent_pids":','.join(map(str,g.pids)),"agent_session_control":"true",
                    "agent_capture":"false","agent_app_preferences":"false"}),
                step("home-stop","Back to Home"),step("stop","Stop routing"),
                step("settings-save","Settings"),step("users-category","Settings category Users & devices"),
                step("profiles","Profiles & backend"),step("save-profile","Save profile",{"profile_path":str(saved_profile)}),
                step("home-final","Back to Home")]
            assert len(steps)==12
        provenance = g.run/"runner-source";provenance.mkdir()
        for name in (Path(__file__).name,"test-connected-macro-studio-acceptance.py",
                     "test-connected-agents-acceptance.py","owned-gui-geometry.py"):
            shutil.copy2(Path(__file__).with_name(name),provenance/name)
        g.evidence["scope"] = "Connected graph global authority/revision/ACK/profile/held release/replica Cancel+ordinary ownership/EOF/draft+Save only; no oldgraphfamily/capacity/i686 execution"
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
    parser.add_argument("--save-tail",action="store_true")
    parser.add_argument("--from-run",type=Path)
    for name in ("executable","x86-executable","runtime"):
        parser.add_argument("--"+name,type=Path,required=True)
    args = parser.parse_args()
    assert args.ready,"Await final frozen READY/exclusive native slot"
    workflow(args)


if __name__=="__main__":
    main()
