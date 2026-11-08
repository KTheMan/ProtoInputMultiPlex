"""Finite connected selected-target magnifier gate; explicit frozen READY only."""
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
    lifecycle = module("mag_access_lifecycle", "test-app-lifecycle-ui.py")
    transport = module("mag_access_transport", "test-connected-agents-acceptance.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    from PIL import Image
    colors = [(45, 105, 165), (160, 65, 95)]
    changed_color = (65, 165, 95)
    ids = ["magnifier-access-a", "magnifier-access-b"]

    class Gate(Base):
        def __init__(self):
            super().__init__(args, "connected-magnifier", offline=False, count=2)
            self.clients, self.raw_clients = [], []
            self.raw, self.mcp, self.descriptor = None, None, None
            self.layers, self.checks, self.frames, self.frame_started = set(), {}, {}, {}
            self.windows = []

        def probes(self):
            return ui.eventually(lambda: super(Gate,self).probes(), lambda reports: True, 3)

        def snapshot(self):
            return ui.eventually(lambda: super(Gate,self).snapshot(), lambda state: True, 3)

        def fixture(self):
            geometry.physical_pixel_context()
            geometry.user32.ClientToScreen.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.POINT)]
            geometry.user32.ShowWindow.argtypes = [wintypes.HWND, ctypes.c_int]
            for i, color in enumerate(colors):
                value = color[0]<<16 | color[1]<<8 | color[2]
                self.owned.append(subprocess.Popen([str(self.images[0]), "probe", "--automation",
                    "--capture-pattern", str(value), "--report", str(self.run/f"probe-{i}.json")],
                    creationflags=subprocess.CREATE_NO_WINDOW))
            self.pids = [p.pid for p in self.owned]
            def windows():
                found = {}
                @geometry.callback_type
                def visit(hwnd, unused):
                    pid = wintypes.DWORD()
                    geometry.user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
                    if pid.value in self.pids and not geometry.user32.GetWindow(hwnd, 4):
                        title = ctypes.create_unicode_buffer(80)
                        geometry.user32.GetWindowTextW(hwnd, title, len(title))
                        if title.value.startswith("ProtoInputMultiPlex Automation Test"):
                            found[pid.value] = int(hwnd)
                    return True
                assert geometry.user32.EnumWindows(visit, 0)
                return found
            found = ui.eventually(windows, lambda result: len(result) == 2, 20)
            for i, pid in enumerate(self.pids):
                hwnd = found[pid]
                assert geometry.user32.SetWindowPos(hwnd, wintypes.HWND(-1), 30+460*i, 40, 420, 360, 0x10)
                point = wintypes.POINT(120, 100)
                assert geometry.user32.ClientToScreen(hwnd, ctypes.byref(point))
                self.points.append([point.x, point.y])
                self.windows.append(hwnd)
            ui.eventually(self.probes, lambda reports: all(r.get("capture_hwnd") for r in reports), 15)

        def reconnect(self):
            for client in self.clients:
                if client.process.poll() is None:
                    client.close()
            for client in self.raw_clients:
                client.close()
            descriptors = list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{self.gui.pid}-*.json"))
            assert len(descriptors) == 1
            self.descriptor = descriptors[0]
            self.mcp = transport.ConnectedMcp(self.images[0], self.descriptor,
                                             self.run/f"mcp-{len(self.clients)}.stderr.log")
            self.clients.append(self.mcp)
            self.raw = transport.OwnedPipe(self.descriptor)
            self.raw_clients.append(self.raw)
            return self.inspect()

        def inspect(self):
            result = self.raw.request("inspect")
            self.project(result)
            return result

        def project(self, result):
            mag = [event["Magnifier"] for event in result["events"]
                   if isinstance(event, dict) and "Magnifier" in event][-1]
            slots = [event["VirtualUsers"]["slots"] for event in result["events"]
                     if isinstance(event, dict) and "VirtualUsers" in event][-1]
            for user in mag["users"]:
                i = user["user"]
                assert user["settings"] == slots[i]["seat"]["magnifier"]
                assert slots[i]["seat"]["id"] == result["profile"]["seats"][i]["id"]
                assert user["settings"] == result["profile"]["seats"][i]["magnifier"]
                if user.get("overlay"):
                    self.layers.add(user["overlay"]["hwnd"])
            for event in result["events"]:
                if isinstance(event, dict) and "State" in event:
                    self.layers.update(user["overlay"]["hwnd"] for user in event["State"] if user.get("overlay"))
            return mag

        def record(self, name, value):
            assert name not in self.checks
            self.checks[name] = value
            self.evidence["checks"] = self.checks
            self.save()

        def raw_command(self, command, error=False):
            sequence = self.raw.sequence
            result = self.raw.request("execute", request={"Magnifier": command})
            assert result["client"] == self.raw.client and result["sequence"] == sequence
            assert bool(result.get("error")) == error
            if not error:
                acks = [event["Magnifier"] for event in result["events"]
                        if isinstance(event, dict) and "Magnifier" in event and event["Magnifier"]["acknowledged"]]
                assert len(acks) == 1 and not acks[0].get("error")
            self.project(result)
            return result

        def command(self, command, error=False):
            result = self.mcp.tool("magnifier", command, error=error)
            if not error:
                assert result["magnifier"]["acknowledged"] and not result["magnifier"].get("error")
            return result

        def deny(self, name, command):
            before = self.inspect()["profile"]
            result = self.command(command, error=True)
            assert self.inspect()["profile"] == before
            path = command.get("path")
            if path:
                assert not Path(path).exists()
            self.record(name, {"error": result, "full_profile_unchanged": True})

        def focus(self, user, target):
            x, y = self.points[target]
            self.mcp.tool("routing_input", {"action": "move_to", "user": user, "x": x, "y": y})
            for pressed in (True, False):
                self.mcp.tool("routing_input", {"action": "button", "user": user,
                                                "button": "primary", "pressed": pressed})

        def ready(self, target=0, after_sequence=None):
            def snapshot():
                return self.project(self.inspect())["users"][0]
            return ui.eventually(snapshot, lambda user: user["source_pid"] == self.pids[target]
                and user["source_hwnd"] == self.probes()[target]["capture_hwnd"]
                and user["capture_sequence"] > (after_sequence if after_sequence is not None else 0)
                and user["status"] == "Ready" and not user["in_flight"], 20)

        def decode(self, path, color):
            with Image.open(path) as image:
                image.load()
                assert image.size == (96, 64)
                rgba = image.convert("RGBA")
                assert rgba.getpixel((48, 32)) == (*color, 255)
                assert rgba.getpixel((24, 20)) == (*color, 255)
                return {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                        "size": list(image.size), "center_RGBA": list(rgba.getpixel((48,32)))}

        def frame(self, name):
            if name in self.frames:
                return True
            hwnd = self.main_hwnd()
            if name not in self.frame_started:
                geometry.resize_owned_client(hwnd, [680,620])
                self.frame_started[name] = time.time()
                return False
            shot = geometry.copy_owned_product_frame(self.run/"ui.png", self.run/(name+".png"),
                                                     hwnd, self.frame_started[name])
            if not shot:
                return False
            self.frames[name] = shot
            return True

        def acks(self):
            path = self.run/"ui.agent.acks.jsonl"
            return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()] if path.exists() else []

    g = Gate()
    expected = None
    done = set()
    gui_transition = None
    first_png = g.run/"owned-a-original.png"
    second_png = g.run/"owned-a-refreshed.png"
    prefs = {"version":1,"appearance_mode":"dark","zoom_percent":100,
             "launch_minimized":False,"minimize_to_tray":False,"close_to_tray":False,
             "launch_at_login":False,"shortcuts":[]}

    def configure(settings):
        before = copy.deepcopy(g.inspect()["profile"])
        result = g.command({"operation":"configure","user":0,"settings":settings})
        snapshot = g.inspect()
        assert g.project(snapshot)["users"][0]["settings"] == settings
        after = copy.deepcopy(before)
        after["seats"][0]["magnifier"] = copy.deepcopy(settings)
        assert snapshot["profile"] == after, "Configure changed another user or unrelated profile fields"
        return result, snapshot

    def initial():
        nonlocal expected
        state = g.reconnect()
        assert not state["policy"]["capture"] and not state["policy"]["session_control"]
        before = state["profile"]
        view = g.mcp.tool("magnifier", {"operation":"inspect"})
        assert view["magnifier"]["users"][0]["settings"] == before["seats"][0]["magnifier"]
        g.focus(0, 0)
        off = copy.deepcopy(before["seats"][0]["magnifier"])
        g.deny("off_configure_no_control", {"operation":"configure","user":0,"settings":off})
        g.deny("refresh_no_capture", {"operation":"refresh","user":0})
        g.deny("export_no_capture", {"operation":"export","user":0,"path":str(g.run/"denied-default.png")})
        expected = copy.deepcopy(g.inspect()["profile"])

    def session_only():
        nonlocal expected
        state = g.reconnect()
        assert state["policy"]["session_control"] and not state["policy"]["capture"]
        off = copy.deepcopy(state["profile"]["seats"][0]["magnifier"])
        off["width"] = 80
        result, snapshot = configure(off)
        expected = copy.deepcopy(snapshot["profile"])
        g.record("session_only_off_configure", {"ack":result["magnifier"],"projection_equal":True})
        g.deny("enabled_configure_without_capture", {"operation":"configure","user":0,"settings":{**off,"mode":"lens"}})
        g.deny("session_only_refresh", {"operation":"refresh","user":0})
        g.deny("session_only_export", {"operation":"export","user":0,"path":str(g.run/"denied-session.png")})

    def both():
        nonlocal expected
        state = g.reconnect()
        assert state["policy"]["session_control"] and state["policy"]["capture"]
        candidate = copy.deepcopy(state["profile"]["seats"][0]["magnifier"])
        candidate.update(mode="lens",source="selected_target",width=96,height=64,zoom=1.0,
                         border=False,auto_refresh=False)
        g.deny("excluded_user_configure", {"operation":"configure","user":1,"settings":candidate})
        g.deny("excluded_user_refresh", {"operation":"refresh","user":1})
        g.deny("excluded_user_export", {"operation":"export","user":1,"path":str(g.run/"denied-user-b.png")})
        g.deny("unknown_user", {"operation":"export","user":99,"path":str(g.run/"denied-unknown.png")})
        g.deny("invalid_configuration", {"operation":"configure","user":0,"settings":{**candidate,"zoom":0}})
        x,y = g.points[0]
        desktop = {**candidate,"source":"desktop","desktop_region":{"left":x-20,"top":y-20,"right":x+12,"bottom":y+12}}
        g.deny("desktop_source_rejected_before_capture", {"operation":"configure","user":0,"settings":desktop})
        result, snapshot = configure(candidate)
        expected = copy.deepcopy(snapshot["profile"])
        ready = g.ready()
        exported = g.command({"operation":"export","user":0,"path":str(first_png)})
        assert exported["magnifier"]["export"]["path"] == str(first_png)
        decoded = g.decode(first_png, colors[0])
        g.record("actual_mcp_configure_and_export", {"configure_ack":result["magnifier"],
                  "ready":ready,"export_ack":exported["magnifier"],"decoded_owned_PNG":decoded})

    def capture_only():
        nonlocal expected
        state = g.reconnect()
        assert state["policy"]["capture"] and not state["policy"]["session_control"]
        current = copy.deepcopy(state["profile"]["seats"][0]["magnifier"])
        g.deny("capture_only_cannot_configure_off", {"operation":"configure","user":0,"settings":{**current,"mode":"off"}})
        old = g.ready()
        report = g.probes()[0]
        assert report["pid"] == g.pids[0]
        color = changed_color[0]<<16 | changed_color[1]<<8 | changed_color[2]
        assert geometry.user32.PostMessageW(report["capture_hwnd"], 0x8051, color, 0)
        ui.eventually(g.probes, lambda reports: reports[0]["capture_color"] == list(changed_color))
        refreshed = g.command({"operation":"refresh","user":0})
        ready = g.ready(after_sequence=old["capture_sequence"])
        original = first_png.read_bytes()
        g.exclusive_read_lock(first_png)
        try:
            failed = g.command({"operation":"export","user":0,"path":str(first_png)}, error=True)
            assert first_png.read_bytes() == original
            assert not list(g.run.glob("*.mux-export.*.tmp"))
            assert g.gui.poll() is None and g.inspect()["profile"] == state["profile"]
        finally:
            g.unlock()
        exported = g.raw_command({"operation":"export","user":0,"path":str(second_png)})
        ack = [event["Magnifier"] for event in exported["events"]
               if isinstance(event,dict) and "Magnifier"in event and event["Magnifier"]["acknowledged"]][0]
        assert ack["export"]["path"] == str(second_png)
        g.record("capture_only_refresh_atomic_failure_tagged_export", {"refresh_ack":refreshed["magnifier"],
            "ready":ready,"locked_export_error":failed,"old_file_bytes_preserved":True,"temporary_files_absent":True,
            "raw_client":exported["client"],"raw_sequence":exported["sequence"],"export_ack":ack,
            "decoded_owned_PNG":g.decode(second_png,changed_color)})
        root = g.windows[0]
        geometry.user32.ShowWindow(root,7)
        try:
            ui.eventually(lambda: g.project(g.inspect())["users"][0], lambda user:
                geometry.user32.IsIconic(root) and user["source_pid"] is None and user["frame_age_ms"] is None)
            g.deny("minimized_missing_source_export", {"operation":"export","user":0,"path":str(g.run/"denied-minimized.png")})
        finally:
            geometry.user32.ShowWindow(root,4)
        ui.eventually(lambda: bool(geometry.user32.IsIconic(root)), lambda iconic:not iconic)
        g.focus(0,0)
        g.ready()
        g.focus(0,1)
        selected = g.ready(target=1)
        g.record("selection_changed_to_owned_b", {"source_pid":selected["source_pid"],"source_hwnd":selected["source_hwnd"]})
        expected = copy.deepcopy(g.inspect()["profile"])

    def restricted():
        state = g.reconnect()
        assert state["policy"]["allowed_pids"] == [g.pids[0]]
        selected = g.project(state)["users"][0]
        assert selected["source_pid"] == g.pids[1]
        g.deny("selected_actual_pid_not_granted_refresh", {"operation":"refresh","user":0})
        g.deny("selected_actual_pid_not_granted_export", {"operation":"export","user":0,"path":str(g.run/"denied-selected-b.png")})
        assert g.inspect()["profile"] == expected

    def disarmed():
        denied = g.mcp.tool("magnifier", {"operation":"export","user":0,
                                          "path":str(g.run/"denied-revoked.png")}, error=True)
        assert not (g.run/"denied-revoked.png").exists()
        assert g.gui.poll() is None and g.snapshot()["profile"] == expected
        mag = ui.eventually(lambda:ui.read_json(g.run/"ui.magnifier.json"),lambda state:True,3)["users"][0]
        assert mag["settings"] == expected["seats"][0]["magnifier"]
        assert mag["overlay"] and geometry.user32.IsWindow(mag["overlay"]["hwnd"])
        assert not g.descriptor.exists()
        g.decode(first_png,colors[0]);g.decode(second_png,changed_color)
        g.record("revocation_preserves_gui_ownership_completed_exports", {"error":denied,
                 "profile_preserved":True,"known_GUI_owned_overlay_exists":True,"completed_exports_preserved":True})
        for client in g.clients:
            if client.process.poll() is None:
                client.close()
        for client in g.raw_clients:
            client.close()

    policy_steps = {"enable":(1,False,False),"session-only":(2,False,True),"both":(3,True,True),
                    "capture-only":(4,True,False),"restricted":(5,True,False),"disable":(6,False,False)}

    def verify(name,sample):
        nonlocal expected,gui_transition
        if name == "start":
            if len(sample["state"]) != 2:
                return False
            g.layers.update(user["overlay"]["hwnd"] for user in sample["state"] if user.get("overlay"))
        if name in policy_steps:
            index,capture,control = policy_steps[name]
            replies = g.acks()
            if len(replies) < index:
                return False
            assert len(replies) == index, "Unexpected extra local policy ACK"
            ack = replies[-1]
            assert not ack["pending"] and not ack["error"]
            assert ack["policy_enabled"] == (name != "disable")
            if name != "disable":
                assert ack["capture"] == capture and ack["session_control"] == control
                assert ack["selected_users"] == ids[:1] and not ack["app_preferences"]
                assert set(ack["allowed_pids"]) == set(g.pids[:1] if name == "restricted" else g.pids)
            sample["committed_policy_ack"] = ack
            if name not in done:
                {"enable":initial,"session-only":session_only,"both":both,"capture-only":capture_only,
                 "restricted":restricted,"disable":disarmed}[name]()
                done.add(name)
            if name in {"both","restricted"} and not g.frame("permission-both" if name=="both"else"permission-capture-only-restricted"):
                return False
        if name == "magnifier":
            if g.snapshot()["profile"] != expected:
                return False
            assert g.inspect()["profile"] == expected
        if name == "apply-mag":
            if gui_transition is None:
                after = copy.deepcopy(expected)
                after["seats"][0]["magnifier"]["mode"] = "docked"
                gui_transition = (copy.deepcopy(expected),after)
            before,after = gui_transition
            actual = g.snapshot()["profile"]
            if actual == before and actual != after:
                return False
            assert actual == after, "GUI Apply lost carried settings or changed another user"
            snapshot = g.inspect()
            assert snapshot["profile"] == after
            expected = after
            g.ready()
            if "gui_apply_authoritative_projection" not in g.checks:
                g.record("gui_apply_authoritative_projection", {"profile":after,"magnifier":g.project(snapshot)})
            if not g.frame("magnifier-committed-draft"):
                return False
        if name == "stop":
            if any(ui.user32.IsWindow(hwnd)for hwnd in g.layers)or(g.descriptor and g.descriptor.exists()):
                return False
            if any(r["held_keys"]or r["buttons"]for r in sample["probes"]):
                return False
        return True

    def after(gate):
        ui.put(gate.run/"final-profile.json",gate.snapshot()["profile"])
        gate.expected_exit=True
        assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0)
        gate.gui.wait(timeout=10)
        assert gate.gui.returncode==0 and not hotkeys.lifecycle_window(gate.gui.pid)
        return True

    try:
        g.fixture()
        profile={"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":g.pids},
            "seats":[{"id":ids[i],"name":"Owned magnifier "+("Alpha"if i==0 else"Beta"),
                "mouse":f"virtual://mouse/{i+1}","keyboard":f"virtual://keyboard/{i+1}","desktop_start":g.points[i],
                "cursor":{"lock_window":False},"magnifier":{"mode":"off","source":"selected_target",
                    "zoom":1.0+i*.5,"width":96,"height":64,"border":False,"auto_refresh":False}}for i in range(2)]}
        step=ui.step
        steps=[step("start","Start routing"),step("settings","Settings"),
            step("agent-category","Settings category Agent access"),step("agent","Agent Access"),
            step("enable","Enable agent access",{"agent_users":ids[0],"agent_pids":",".join(map(str,g.pids)),
                "agent_capture":"false","agent_session_control":"false","agent_app_preferences":"false"}),
            step("grant-control","Agent session control permission"),step("session-only","Apply access policy"),
            step("grant-capture","Agent capture permission"),step("both","Apply access policy"),
            step("home-mag","Back to Home"),step("select-a","Select user 1"),step("configure-a","Configure user"),
            step("magnifier","Magnifier"),step("docked","Magnifier Docked"),step("apply-mag","Apply magnifier settings"),
            step("home-capture","Back to Home"),step("settings-capture","Settings"),step("agent-capture","Agent Access"),
            step("revoke-control","Agent session control permission"),step("capture-only","Apply access policy"),
            step("restricted","Apply access policy",{"agent_pids":str(g.pids[0])}),
            step("disable","Disable agent access"),step("home-stop","Back to Home"),step("stop","Stop routing")]
        assert len(steps)==24 and not any({"magnifier_user","selected_user"}&set(s.get("fields",{}))for s in steps)
        provenance=g.run/"runner-source";provenance.mkdir()
        for name in [Path(__file__).name,"test-connected-agents-acceptance.py","owned-gui-geometry.py"]:
            shutil.copy2(Path(__file__).with_name(name),provenance/name)
        g.evidence["scope"]="Connected selected-target magnifier permissions/ACK/profile/draft/PNG export; no Desktop capture, forced races, old algorithm matrix or i686 execution"
        g.execute(profile,prefs,steps,verify,lambda gate:gate.main_hwnd()is not None,after)
        g.evidence.update(result="PENDING INDEPENDENT VISUAL REVIEW",control_state_result="PASS",captures=g.frames)
        g.save();return g.run
    finally:
        for client in g.clients:
            if client.process.poll()is None:
                client.close()
        for client in g.raw_clients:
            client.close()
        g.evidence["connected_mcp_closed"]=all(c.process.poll()is not None for c in g.clients)
        g.evidence["raw_handles_closed"]=all(c.handle is None for c in g.raw_clients)
        g.close();lifecycle.remove_startup_key(g.startup_key)


def main():
    parser=argparse.ArgumentParser();parser.add_argument("--ready",action="store_true")
    for name in("executable","x86-executable","runtime"):
        parser.add_argument("--"+name,type=Path,required=True)
    args=parser.parse_args();assert args.ready,"Await final frozen READY/exclusive slot"
    workflow(args)


if __name__=="__main__":
    main()
