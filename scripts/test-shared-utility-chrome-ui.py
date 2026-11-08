"""Finite stopped shared utility chrome gate; frozen READY required."""
import argparse
import copy
import hashlib
import importlib.util
from pathlib import Path
import shutil
import time


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def workflow(args):
    lifecycle = module("chrome_lifecycle", "test-app-lifecycle-ui.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()

    class Gate(Base):
        def __init__(self):
            super().__init__(args,"shared-utility-chrome",offline=True,count=0)
            self.frames,self.started={},{}

        def frame(self,name,size):
            if name in self.frames:return True
            hwnd=self.main_hwnd()
            if name not in self.started:
                geometry.resize_owned_client(hwnd,size);self.started[name]=time.time();return False
            current=geometry.geometry(hwnd)
            if any(abs(a-b)>2 for a,b in zip(current["client_logical"],size)):return False
            frame=geometry.copy_owned_product_frame(self.run/"ui.png",self.run/(name+".png"),hwnd,self.started[name])
            if not frame:return False
            frame["geometry"]=current;self.frames[name]=frame;return True

        def graph(self):
            return ui.read_json(self.run/"ui.graph.json")

    g=Gate()
    profile={"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":[987301,987302]},
        "seats":[{"id":f"chrome-owned-{i}","name":"Owned Alpha" if i==0 else "Owned Beta",
            "mouse":f"virtual://mouse/{i+1}","keyboard":f"virtual://keyboard/{i+1}",
            "cursor":{"color":[70,190,160] if i==0 else [230,140,60]}}for i in range(2)]}
    prefs={"version":1,"appearance_mode":"dark","zoom_percent":100,"launch_minimized":False,
        "minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,"shortcuts":[]}
    step=ui.step
    fields={"graph_source":"chrome-owned-0","graph_target":"chrome-owned-1",
        "graph_mouse":"true","graph_keyboard":"false","graph_text":"false"}
    steps=[step("settings","Settings"),step("application","Application preferences"),
        step("home-app","Back to Home"),step("settings-agent","Settings"),
        step("agent-category","Settings category Agent access"),step("agent","Agent Access"),
        step("home-agent","Back to Home"),step("settings-graph","Settings"),
        step("automation-category","Settings category Automation"),step("graph","Replication Graph"),
        step("queue-open","Queue diagnostics"),step("queue-close","Queue diagnostics"),
        step("connect-details","Connect users"),step("connect","Connect selected users",fields),
        step("duplicate","Connect selected users"),step("home-graph","Back to Home")]
    assert len(steps)==16
    expected=None;preferences_bytes=None;draft=None

    def verify(name,sample):
        nonlocal expected,preferences_bytes,draft
        current=g.snapshot()["profile"]
        if expected is None:
            expected=copy.deepcopy(current);preferences_bytes=g.preferences_path.read_bytes()
        assert current==expected,"Chrome/navigation mutated the complete profile"
        assert g.preferences_path.read_bytes()==preferences_bytes,"Chrome/navigation mutated preferences"
        assert not(g.run/"ui.state.json").exists(),"Stopped gate unexpectedly created a routing worker"
        if name=="application" and not g.frame("application-chrome-normal",[540,480]):return False
        if name=="agent":
            policy=ui.read_json(g.run/"ui.agent.json")
            assert not any(policy[k]for k in("policy_enabled","session_control","capture","app_preferences","clients","pending"))
            sample["unchanged_disabled_agent_policy"]=policy
            if not g.frame("agent-chrome-minimum",[320,360]):return False
        if name=="queue-open":
            state=g.graph();assert not state["pending"] and not state["draft"]["edges"] and not state["applied"]["edges"]
            if not g.frame("graph-queue-open-normal",[860,680]):return False
        if name=="queue-close":
            state=g.graph();assert not state["pending"] and not state["draft"]["edges"] and not state["applied"]["edges"]
        if name=="connect":
            state=g.graph()
            if len(state["draft"]["edges"])!=1:return False
            assert not state["applied"]["edges"] and not state["pending"] and not state["error"]
            edge=state["draft"]["edges"][0]
            assert edge["source"]=="chrome-owned-0" and edge["target"]=="chrome-owned-1"
            assert edge["mouse"] and not edge["keyboard"] and not edge["text"]
            draft=copy.deepcopy(state["draft"])
        if name=="duplicate":
            state=g.graph()
            if not state["error"]:return False
            assert state["draft"]==draft and not state["applied"]["edges"] and not state["pending"]
            sample["preserved_draft_with_real_duplicate_error"]=state
            if not g.frame("graph-closed-diagnostics-error-minimum",[320,360]):return False
        sample["full_profile_preferences_unchanged"]=True
        return True

    def after(gate):
        assert gate.snapshot()["profile"]==expected
        assert gate.preferences_path.read_bytes()==preferences_bytes
        ui.put(gate.run/"final-profile.json",expected)
        gate.expected_exit=True
        assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0)
        gate.gui.wait(timeout=10)
        assert gate.gui.returncode==0 and not hotkeys.lifecycle_window(gate.gui.pid)
        return True

    try:
        provenance=g.run/"runner-source";provenance.mkdir()
        shutil.copy2(Path(__file__),provenance/Path(__file__).name)
        g.evidence["scope"]="Stopped shared utility chrome and queue-disclosure presentation only; no worker, target apps, MCP, routing, Agent enable or permissions change. Local graph draft edit is deliberate; complete profile/preferences remain unchanged."
        g.execute(profile,prefs,steps,verify,lambda gate:gate.main_hwnd()is not None,after)
        assert len(g.frames)==4
        g.evidence.update(result="PENDING INDEPENDENT VISUAL REVIEW",control_profile_result="PASS",captures=g.frames)
        g.save();return g.run
    finally:
        g.close();lifecycle.remove_startup_key(g.startup_key)


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument("--ready",action="store_true")
    for name in("executable","x86-executable","runtime"):parser.add_argument("--"+name,type=Path,required=True)
    args=parser.parse_args();assert args.ready,"Await final source/producer/exclusive ROOT READY"
    workflow(args)


if __name__=="__main__":main()
