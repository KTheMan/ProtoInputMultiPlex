"""Finite shared-GUI annotation gate; no native launch before frozen READY."""
import argparse
import copy
import json
import ctypes
from ctypes import wintypes
import importlib.util
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


MATRIX = [
    "read-only Inspect with control off preserves current semantics",
    "Configure without session_control denied atomically",
    "ClearUser/ClearAll without session_control denied atomically",
    "real A-only control grant and matching successful Configure A ACK",
    "Configure excluded B denied atomically",
    "ClearUser excluded B denied with ink/holds preserved",
    "partial-grant ClearAll and all-user clear shortcut Configure denied atomically",
    "invalid A settings preserve committed settings/ink/holds",
    "unknown numeric user denied without mutation",
    "Configure rejects selected/disabled active claims, unrelated B hold permits A",
    "Configure rejects open stroke; Clear closes ink but raw claim waits source UP",
    "successful Configure persists across periodic state and GUI Draw draft",
    "ClearUser A removes only A actual ink and preserves routing holds",
    "all-active grant ClearAll clears ink only",
    "export and magnifier mutation remain unsupported",
    "GUI-origin annotation configuration projects to connected profile",
    "MCP EOF preserves GUI session; normal Stop and owned cleanup",
]


def annotation_event(reply):
    """Extract exactly one actual annotation command event, never a snapshot."""
    events = reply.get("events", [])
    matches = [event["Annotation"] for event in events
               if isinstance(event, dict) and "Annotation" in event
               and event["Annotation"]["acknowledged"]]
    assert len(matches) == 1, "Expected one matching annotation command event"
    state = matches[0]
    assert state["acknowledged"], "Periodic annotation state is not a command ACK"
    return state


def matching_reply(reply, client, sequence, *, error=False):
    assert reply["client"] == client and reply["sequence"] == sequence
    assert bool(reply.get("error")) == error
    return reply


def unchanged(before, after, message="Rejected operation changed authoritative state"):
    assert after == before, message


def replace_user_annotation(profile, stable_id, settings):
    """Build an independent expected profile by stable identity, not row order."""
    expected = copy.deepcopy(profile)
    seats = [seat for seat in expected["seats"] if seat["id"] == stable_id]
    assert len(seats) == 1
    seats[0]["annotation"] = copy.deepcopy(settings)
    return expected


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def workflow(args):
    lifecycle = module("anno_access_lifecycle", "test-app-lifecycle-ui.py")
    connected = module("anno_access_transport", "test-connected-agents-acceptance.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()

    class Gate(Base):
        def __init__(self):
            super().__init__(args, "connected-annotation", offline=False, count=2)
            self.clients = []
            self.raw_clients = []
            self.descriptor = None
            self.main_client = None
            self.raw = None
            self.layers = set()
            self.checks = {}
            self.frames = {}
            self.frame_started = {}

        def fixture(self):
            for i in range(2):
                process = subprocess.Popen([str(self.images[0]), "probe", "--automation", "--report",
                                            str(self.run/f"probe-{i}.json")],
                                           creationflags=subprocess.CREATE_NO_WINDOW)
                self.owned.append(process)
            self.pids = [p.pid for p in self.owned]
            # Enumerate only exact own PIDs; no unrelated target/title inventory.
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
            owned = ui.eventually(windows, lambda value: len(value) == 2, 20)
            geometry.user32.ClientToScreen.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.POINT)]
            for i, pid in enumerate(self.pids):
                hwnd = owned[pid]
                assert geometry.user32.SetWindowPos(hwnd, wintypes.HWND(-1), 30+460*i, 40,
                                                     420, 360, 0x10)
                point = wintypes.POINT(120, 100)
                assert geometry.user32.ClientToScreen(hwnd, ctypes.byref(point))
                self.points.append([point.x, point.y])

        def reconnect(self):
            for client in self.clients:
                if client.process.poll() is None:
                    client.close()
            for client in self.raw_clients:
                client.close()
            paths = list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{self.gui.pid}-*.json"))
            assert len(paths) == 1, "Discover only our GUI PID descriptor"
            self.descriptor = paths[0]
            self.main_client = connected.ConnectedMcp(self.images[0], self.descriptor,
                                                     self.run/f"mcp-{len(self.clients)}.stderr.log")
            self.clients.append(self.main_client)
            self.raw = connected.OwnedPipe(self.descriptor)
            self.raw_clients.append(self.raw)
            return self.inspect()

        def request(self, request, error=False, client=None):
            client = client or self.raw
            sequence = client.sequence
            result = client.request("execute", request=request)
            matching_reply(result, client.client, sequence, error=error)
            return result

        def inspect(self):
            result = self.raw.request("inspect")
            self.project(result)
            return result

        def project(self, result):
            annotation = [e["Annotation"] for e in result["events"]
                          if isinstance(e, dict) and "Annotation" in e][-1]
            slots = [e["VirtualUsers"]["slots"] for e in result["events"]
                     if isinstance(e, dict) and "VirtualUsers" in e][-1]
            for user in annotation["users"]:
                i = user["user"]
                assert user["settings"] == slots[i]["seat"]["annotation"]
                assert slots[i]["seat"]["id"] == result["profile"]["seats"][i]["id"]
                assert user["settings"] == result["profile"]["seats"][i]["annotation"]
            if annotation.get("overlay"):
                self.layers.add(annotation["overlay"]["hwnd"])
            for event in result["events"]:
                if isinstance(event, dict) and "State" in event:
                    self.layers.update(s["overlay"]["hwnd"] for s in event["State"] if s.get("overlay"))
            return annotation

        def signature(self, result):
            annotation = self.project(result)
            return {"profile": result["profile"], "users": annotation["users"],
                    "revision": annotation["revision"]}

        def check(self, name, value):
            assert name not in self.checks
            self.checks[name] = value
            self.evidence["checks"] = self.checks
            self.save()

        def mcp_annotation(self, command, error=False):
            return self.main_client.tool("annotation", command, error=error)

        def deny(self, name, command, tool="annotation"):
            before = self.signature(self.inspect())
            result = self.main_client.tool(tool, command, error=True)
            after = self.signature(self.inspect())
            unchanged(before, after)
            self.check(name, {"denied": True, "error": result, "authoritative_unchanged": True})

        def annotation(self, command, error=False):
            result = self.request({"Annotation": command}, error=error)
            if not error:
                state = annotation_event(result)
                assert not state.get("error")
                assert state["users"] == self.project(result)["users"]
            return result

        def configure(self, settings, error=False):
            return self.annotation({"operation": "configure", "user": 0, "settings": settings}, error)

        def packet(self, user, kind, flags, x=0, y=0, data=0, client=None, error=False):
            device = 0xf1000002+2*user+kind
            return self.request({"Input": {"device": device, "kind": kind, "flags": flags,
                                           "x": x, "y": y, "data": data}}, error, client)

        def key(self, user, vk, pressed, client=None, error=False):
            scan = {116: 63, 117: 64, 118: 65, 119: 66}[vk]
            return self.packet(user, 1, 0 if pressed else 1, scan, data=vk,
                               client=client, error=error)

        def move(self, user, offset=0):
            x, y = self.points[user]
            return self.request({"MoveTo": {"user": user, "x": x+offset, "y": y}})

        def mark(self, user):
            self.move(user)
            self.packet(user, 0, 64)
            self.move(user, 25)
            self.packet(user, 0, 128)
            state = self.project(self.inspect())
            assert state["users"][user]["strokes"] and not state["users"][user]["open"]
            return state

        def focus(self, user):
            self.move(user)
            self.packet(user, 0, 1)
            self.packet(user, 0, 2)

        def busy(self, name, settings):
            before = self.signature(self.inspect())
            result = self.configure(settings, error=True)
            unchanged(before, self.signature(result))
            self.check(name, {"client": result["client"], "sequence": result["sequence"],
                              "error": result["error"], "state_unchanged": True})

        def frame(self, name):
            if name in self.frames:
                return True
            hwnd = self.main_hwnd()
            if name not in self.frame_started:
                geometry.resize_owned_client(hwnd, [680, 620])
                self.frame_started[name] = time.time()
                return False
            shot = geometry.copy_owned_product_frame(self.run/"ui.png", self.run/(name+".png"),
                                                     hwnd, self.frame_started[name])
            if not shot:
                return False
            self.frames[name] = shot
            return True

    g = Gate()
    ids = ["annotation-access-a", "annotation-access-b"]
    profile = None
    expected = None
    phases = set()
    retained = {}
    prefs = {"version": 1, "appearance_mode": "dark", "zoom_percent": 100,
             "launch_minimized": False, "minimize_to_tray": False, "close_to_tray": False,
             "launch_at_login": False, "shortcuts": []}

    def no_control():
        nonlocal expected
        initial = g.reconnect()
        assert not initial["policy"]["session_control"] and not initial["policy"]["capture"]
        for user in range(2):
            g.focus(user)
            g.mark(user)
        state = g.inspect()
        expected = copy.deepcopy(state["profile"])
        viewed = g.mcp_annotation({"operation": "inspect"})
        assert viewed["annotation"]["users"] == g.project(state)["users"]
        g.check("inspect_control_off", {"read_only": True, "projection_equal": True})
        candidate = copy.deepcopy(expected["seats"][0]["annotation"])
        candidate["width"] = 17
        g.deny("configure_control_off", {"operation": "configure", "user": 0, "settings": candidate})
        g.deny("clear_user_control_off", {"operation": "clear_user", "user": 0})
        g.deny("clear_all_control_off", {"operation": "clear_all"})

    def partial():
        nonlocal expected
        state = g.reconnect()
        assert state["policy"]["session_control"] and state["policy"]["user_ids"] == [ids[0]]
        # GUI-preseeded all-user shortcut must not bypass the reduced grant.
        before = g.signature(state)
        denied = g.main_client.tool("routing_input", {"action": "key", "user": 0,
                                                      "key": "f8", "pressed": True}, error=True)
        unchanged(before, g.signature(g.inspect()))
        g.check("preexisting_global_shortcut_partial_denied", {"error": denied, "ink_profile_unchanged": True})
        candidate = copy.deepcopy(state["profile"]["seats"][0]["annotation"])
        g.deny("global_shortcut_configuration_partial_denied",
               {"operation": "configure", "user": 0, "settings": candidate})
        candidate.update(clear_all=False, clear_key=118, width=21)
        for name, command in [
            ("excluded_b_configure", {"operation": "configure", "user": 1, "settings": candidate}),
            ("excluded_b_clear", {"operation": "clear_user", "user": 1}),
            ("partial_clear_all", {"operation": "clear_all"}),
            ("invalid_settings", {"operation": "configure", "user": 0, "settings": {**candidate, "width": 0}}),
            ("unknown_user", {"operation": "clear_user", "user": 99})]:
            g.deny(name, command)
        unknown = g.request({"Annotation": {"operation": "clear_user", "user": 99}}, error=True)
        unchanged(before["profile"], unknown["profile"])
        configured = g.configure(candidate)
        g.check("configure_matching_ack", {"client": configured["client"], "sequence": configured["sequence"],
                                            "annotation": annotation_event(configured), "projection_equal": True})
        holder = connected.OwnedPipe(g.descriptor)
        g.raw_clients.append(holder)
        assert holder.client != g.raw.client
        g.key(0, 116, True, client=holder)
        ui.eventually(g.probes, lambda rs: 116 in rs[0]["held_keys"])
        g.busy("other_actor_selected_key_busy", {**candidate, "width": 22})
        assert 116 in g.probes()[0]["held_keys"]
        g.key(0, 116, False, client=holder)
        ui.eventually(g.probes, lambda rs: 116 not in rs[0]["held_keys"])
        holder.close()
        output = copy.deepcopy(g.inspect()["profile"]["seats"][0]["output"])
        disabled = copy.deepcopy(output)
        disabled["buttons"][4] = {"type": "disabled"}
        g.main_client.tool("routing_output", {"operation": "configure", "user": 0, "settings": disabled})
        g.packet(0, 0, 256)
        g.busy("disabled_source_busy", {**candidate, "width": 22})
        assert not g.probes()[0]["buttons"]
        g.packet(0, 0, 512)
        g.main_client.tool("routing_output", {"operation": "configure", "user": 0, "settings": output})
        g.move(0)
        g.packet(0, 0, 64)
        assert g.project(g.inspect())["users"][0]["open"]
        g.busy("open_stroke_busy", {**candidate, "width": 22})
        cleared = g.annotation({"operation": "clear_user", "user": 0})
        assert not g.project(cleared)["users"][0]["open"]
        g.busy("consumed_button_after_clear_busy", {**candidate, "width": 22})
        g.packet(0, 0, 128)
        g.key(0, 118, True)
        g.busy("consumed_clear_key_busy", {**candidate, "width": 22})
        g.key(0, 118, False)
        candidate["width"] = 27
        configured = g.mcp_annotation({"operation": "configure", "user": 0, "settings": candidate})
        assert configured["annotation"]["acknowledged"] and not configured["annotation"].get("error")
        committed = g.inspect()
        assert g.project(committed)["users"][0]["settings"] == candidate
        expected = copy.deepcopy(committed["profile"])
        g.check("post_up_mcp_configure_and_periodic", {"annotation": configured["annotation"], "profile": expected})

    def all_users():
        nonlocal expected
        state = g.reconnect()
        assert set(state["policy"]["user_ids"]) == set(ids) and state["policy"]["session_control"]
        other = connected.OwnedPipe(g.descriptor)
        g.raw_clients.append(other)
        g.key(1, 117, True, client=other)
        ui.eventually(g.probes, lambda rs: 117 in rs[1]["held_keys"])
        candidate = copy.deepcopy(state["profile"]["seats"][0]["annotation"])
        candidate.update(tool="brush", width=30, clear_key=119, clear_all=True)
        configured = g.configure(candidate)
        assert 117 in g.probes()[1]["held_keys"]
        g.check("unrelated_b_hold_allows_configure_a", {"ack": annotation_event(configured), "b_held": True})
        g.mark(0)
        old_b = copy.deepcopy(g.project(g.inspect())["users"][1])
        cleared = g.mcp_annotation({"operation": "clear_user", "user": 0})
        assert cleared["annotation"]["acknowledged"] and not cleared["annotation"].get("error")
        ink = g.project(g.inspect())
        assert ink["users"][0]["strokes"] == 0 and ink["users"][1] == old_b
        assert ink["overlay"]["visible"] and ui.user32.IsWindow(ink["overlay"]["hwnd"])
        assert 117 in g.probes()[1]["held_keys"]
        g.check("clear_user_ownership", {"ack": cleared["annotation"], "other_actor_hold_preserved": True})
        g.mark(0)
        cleared = g.mcp_annotation({"operation": "clear_all"})
        assert cleared["annotation"]["acknowledged"] and not cleared["annotation"].get("error")
        ink = g.project(g.inspect())
        assert all(not u["strokes"] and not u["segments"] for u in ink["users"])
        assert 117 in g.probes()[1]["held_keys"]
        g.check("clear_all_authorized", {"ack": cleared["annotation"], "other_actor_hold_preserved": True})
        for user in range(2):
            g.mark(user)
        g.key(0, 119, True)
        ink = g.project(g.inspect())
        assert all(not u["strokes"] for u in ink["users"])
        g.key(0, 119, False)
        assert 117 in g.probes()[1]["held_keys"]
        g.check("global_shortcut_all_grant", {"cleared_all": True, "other_actor_hold_preserved": True})
        path = g.run/"unsupported-export.png"
        g.deny("export_still_unsupported", {"operation": "export", "user": 0,
                                            "path": str(path), "kind": "ink_only"})
        assert not path.exists()
        g.deny("magnifier_still_unsupported", {"operation": "refresh", "user": 0}, tool="magnifier")
        expected = copy.deepcopy(g.inspect()["profile"])
        other.close()
        ui.eventually(g.probes, lambda rs: 117 not in rs[1]["held_keys"])
        g.main_client.close()
        g.raw.close()
        assert g.gui.poll() is None
        ui.eventually(lambda: g.snapshot()["profile"], lambda p: p == expected)
        assert len(ui.read_json(g.run/"ui.state.json")) == 2
        g.check("mcp_eof_preserves_gui", {"gui_alive": True, "authoritative_profile_preserved": True,
                                          "other_actor_release_observed": True})

    def verify(name, sample):
        nonlocal expected
        if name == "start":
            if len(sample["state"]) != 2:
                return False
            g.layers.update(s["overlay"]["hwnd"] for s in sample["state"] if s.get("overlay"))
        if name in {"enable", "apply-partial", "apply-all"}:
            agent = ui.read_json(g.run/"ui.agent.json")
            if agent["pending"] or not agent["policy_enabled"]:
                return False
            control = name != "enable"
            wanted = ids[:1] if name == "apply-partial" else ids
            if agent["session_control"] != control or set(agent["selected_users"]) != set(wanted):
                return False
            assert not agent["capture"] and set(agent["allowed_pids"]) == set(g.pids)
            if name not in phases:
                {"enable": no_control, "apply-partial": partial, "apply-all": all_users}[name]()
                phases.add(name)
            sample["policy"] = agent
            if name == "apply-partial" and not g.frame("agent-annotation-permission"):
                return False
        if name in {"draw", "eraser"}:
            if g.snapshot()["profile"] != expected:
                return False
            state = g.inspect()
            assert state["profile"] == expected
            assert state["profile"]["seats"][0]["annotation"]["width"] == 27
            assert state["profile"]["seats"][0]["annotation"]["clear_key"] == 118
        if name == "apply-draw":
            if "gui-apply-transition" not in retained:
                after = copy.deepcopy(expected)
                after["seats"][0]["annotation"]["tool"] = "eraser"
                retained["gui-apply-transition"] = (copy.deepcopy(expected), after)
            before, after = retained["gui-apply-transition"]
            current = g.snapshot()["profile"]
            if current == before and current != after:
                return False
            unchanged(after, current, "GUI Apply lost agent-owned draft fields or changed another user")
            state = g.inspect()
            assert state["profile"] == after
            expected = after
            if "gui_apply_authoritative_projection" not in g.checks:
                g.check("gui_apply_authoritative_projection", {"profile": after,
                        "annotation": g.project(state), "no_feature_recipient_fields": True})
            if not g.frame("draw-committed-agent-draft"):
                return False
        if name == "stop":
            if any(ui.user32.IsWindow(hwnd) for hwnd in g.layers):
                return False
            if g.descriptor and g.descriptor.exists():
                return False
            if any(r["held_keys"] or r["buttons"] for r in sample["probes"]):
                return False
        return True

    def after(gate):
        ui.put(gate.run/"final-profile.json", gate.snapshot()["profile"])
        gate.expected_exit = True
        assert geometry.user32.PostMessageW(gate.main_hwnd(), 0x10, 0, 0)
        gate.gui.wait(timeout=10)
        assert gate.gui.returncode == 0 and not hotkeys.lifecycle_window(gate.gui.pid)
        return True

    try:
        g.fixture()
        profile = {"version": 1, "concurrent": True,
                   "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": g.pids},
                   "seats": [{"id": ids[i], "name": "Owned annotation "+("Alpha" if i == 0 else "Beta"),
                              "mouse": f"virtual://mouse/{i+1}", "keyboard": f"virtual://keyboard/{i+1}",
                              "desktop_start": g.points[i], "cursor": {"lock_window": False},
                              "annotation": {"tool": "brush", "activator": "x1", "width": 9+i,
                                             "color": [40, 180, 120] if i == 0 else [210, 80, 50],
                                             "alpha": 180, "clear_key": 119 if i == 0 else None,
                                             "clear_all": i == 0}} for i in range(2)]}
        step = ui.step
        initial_policy = {"agent_users": ",".join(ids), "agent_pids": ",".join(map(str, g.pids)),
                          "agent_capture": "false", "agent_session_control": "false", "agent_app_preferences": "false"}
        steps = [step("start", "Start routing"), step("settings", "Settings"), step("agent", "Agent Access"),
                 step("enable", "Enable agent access", initial_policy),
                 step("control", "Agent session control permission"),
                 step("apply-partial", "Apply access policy", {"agent_users": ids[0]}),
                 step("home-draw", "Back to Home"), step("select-a", "Select user 1"),
                 step("configure-a", "Configure user"), step("draw", "Draw"),
                 step("eraser", "Annotation Eraser"), step("apply-draw", "Apply annotation settings"),
                 step("home-all", "Back to Home"), step("settings-all", "Settings"),
                 step("agent-all", "Agent Access"),
                 step("apply-all", "Apply access policy", {"agent_users": ",".join(ids)}),
                 step("home-stop", "Back to Home"), step("stop", "Stop routing")]
        assert len(steps) == 18
        assert not any({"annotation_user", "selected_user"} & set(s.get("fields", {})) for s in steps)
        provenance = g.run/"runner-source"
        provenance.mkdir()
        for filename in [Path(__file__).name, "test-connected-agents-acceptance.py", "owned-gui-geometry.py"]:
            shutil.copy2(Path(__file__).with_name(filename), provenance/filename)
        g.evidence["scope"] = "Connected annotation authorization/ACK/profile/draft/ink-count integration; own GUI pixels only, no ink readback/export/capture/algorithm or broad delivery replay; x64 probes only, i686 unused"
        g.execute(profile, prefs, steps, verify, lambda gate: gate.main_hwnd() is not None, after)
        g.evidence.update(result="PENDING INDEPENDENT VISUAL REVIEW", control_state_result="PASS", captures=g.frames)
        g.save()
        return g.run
    finally:
        for client in g.clients:
            if client.process.poll() is None:
                client.close()
        for client in g.raw_clients:
            client.close()
        g.evidence["connected_mcp_closed"] = all(c.process.poll() is not None for c in g.clients)
        g.evidence["raw_handles_closed"] = all(c.handle is None for c in g.raw_clients)
        g.close()
        lifecycle.remove_startup_key(g.startup_key)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--matrix", action="store_true")
    parser.add_argument("--ready", action="store_true")
    for name in ("executable", "x86-executable", "runtime"):
        parser.add_argument("--"+name, type=Path)
    args = parser.parse_args()
    if args.matrix:
        print(json.dumps({"status": "PREPARATION ONLY", "groups": MATRIX}, indent=2))
    else:
        assert args.ready and args.executable and args.x86_executable and args.runtime, "Await final READY/exclusive slot"
        workflow(args)


if __name__ == "__main__":
    main()
