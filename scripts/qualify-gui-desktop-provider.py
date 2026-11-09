"""One disposable hosted production-virtual-provider gate; public decisions only.

No physical input, authority-file edits, forced termination, action retry or replay.
Imports the existing owned GUI/CDP/receipt and MCP stdio helpers. Raw evidence and
credentials stay in this fresh private run root. This is an opt-in controller.
"""
import argparse
import ctypes
from ctypes import wintypes
import http.server
import importlib.util
import json
import os
from pathlib import Path
import queue
import re
import shutil
import struct
import subprocess
import threading
import time
from urllib.parse import urlsplit


def module(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


base = module("provider_pair", "qualify-gui-pair.py")
mcp_helpers = module("provider_mcp", "test-mcp.py")
require = base.require


class CommandMcp(mcp_helpers.Client):
    def __init__(self, executable, descriptor, log):
        self.stderr = open(log, "x", encoding="utf-8")
        self.process = subprocess.Popen([str(executable), "mcp", "--gui-connection", str(descriptor)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr,
            text=True, encoding="utf-8", creationflags=subprocess.CREATE_NO_WINDOW)
        self.messages, self.next_id, self.latest = queue.Queue(128), 0, False
        threading.Thread(target=self.read, daemon=True).start()
        value = self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "owned-provider-gate", "version": "1"}})
        require(value["capabilities"]["tools"] is not None)
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def read(self):
        try:
            for line in self.process.stdout:
                require(len(line) <= 4 * 1024 * 1024)
                self.messages.put_nowait(json.loads(line))
        except BaseException as error:
            self.messages.put_nowait(error)
        finally:
            if not self.messages.full():
                self.messages.put_nowait(EOFError())


def one_owned_click_batch(descriptor):
    """Existing GUI command framing, one fixed three-packet virtual actor only."""
    data = base.read(descriptor)
    require(data["pipe"].startswith("\\\\.\\pipe\\ProtoInputMultiPlex.") and len(data["pipe"]) < 512)
    require(re.fullmatch("[0-9a-fA-F]{64}", data["token"]) is not None)
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
        ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
    kernel.CreateFileW.restype = wintypes.HANDLE
    kernel.SetNamedPipeHandleState.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p, ctypes.c_void_p]
    kernel.ReadFile.argtypes = [wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p]
    kernel.WriteFile.argtypes = kernel.ReadFile.argtypes
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = kernel.CreateFileW(data["pipe"], 0xc0000000, 0, None, 3, 0, None)
    require(handle != ctypes.c_void_p(-1).value)
    end = time.monotonic() + 15
    try:
        mode = wintypes.DWORD(1)
        require(kernel.SetNamedPipeHandleState(handle, ctypes.byref(mode), None, None))
        def send(value):
            raw = json.dumps(value).encode("utf-8")
            raw = struct.pack(">I", len(raw)) + raw
            offset = 0
            while offset < len(raw):
                require(time.monotonic() < end)
                part = ctypes.create_string_buffer(raw[offset:]); size = wintypes.DWORD()
                ok = kernel.WriteFile(handle, part, len(raw)-offset, ctypes.byref(size), None)
                require(ok or ctypes.get_last_error() == 232)
                offset += size.value
                if not size.value:
                    time.sleep(.005)
        def exact(count):
            out = bytearray()
            while len(out) < count:
                require(time.monotonic() < end)
                part = ctypes.create_string_buffer(count-len(out)); size = wintypes.DWORD()
                ok = kernel.ReadFile(handle, part, len(part), ctypes.byref(size), None)
                require(ok or ctypes.get_last_error() == 232)
                out.extend(part.raw[:size.value])
                if not size.value:
                    time.sleep(.005)
            return bytes(out)
        def receive():
            size = struct.unpack(">I", exact(4))[0]
            require(0 < size <= 4 * 1024 * 1024)
            return json.loads(exact(size))["reply"]
        send({"type": "connect", "token": data["token"]})
        connected = receive()
        require(connected["acknowledged"] and connected["client"] > 0 and connected["sequence"] == 0 and not connected.get("error"))
        # Virtual mouse 1, explicitly declared by this fixture's GUI profile.
        packet = {"device": 0xf1000002, "kind": 0, "x": 0, "y": 0, "data": 0}
        send({"type": "execute", "sequence": 1, "request": {"Batch": [
            dict(packet, flags=1), dict(packet, flags=2), dict(packet, flags=0, x=32)]}})
        reply = receive()
        require(reply["acknowledged"] and reply["client"] == connected["client"] and reply["sequence"] == 1 and not reply.get("error"))
    finally:
        require(kernel.CloseHandle(handle))


class ProviderPair(base.Pair):
    def __init__(self, args):
        super().__init__(args)
        self.end = self.start + 420
        self.private = args.run.resolve()
        self.paths, self.issued, self.native_runs = {}, set(), []
        self.result = {"schema": "pim.desktop-provider.hosted.v1", "source_commit": args.commit,
            "status": "FAIL", "scope": "two explicitly bound production virtual seats; native DLL and Rust/WASM; owned native targets",
            "full_sdk_complete": False, "release_qualified": False, "hardware_qualified": False,
            "same_process_isolation_qualified": False, "no_action_retry_or_replay": True,
            "provider_core_gap": "unit_coverage_only; prior_cooperative_actual_gap_separate",
            "worker_journal_overflow": "unit_coverage_only"}

    def launch_status(self):
        progress, modified = self.live_json("gui_progress", self.ui / "prepare.progress.json")
        require(time.time()-modified < 2 and progress["io"]["fault"] is None)
        value, modified = self.live_json("gui_launch", self.ui / "prepare.launch.json")
        require(time.time()-modified < 2)
        return value

    def sdk_status(self):
        return self.launch_status()["setup"]["application_events"]

    def bound(self, count):
        status = self.sdk_status()
        return status if status.get("desktop_bound_users") == count and status.get("desktop_pending_users") == 0 else None

    def target(self, index):
        def sample():
            raw, modified = base.shared_live_bytes(self.private / f"target-{index}.json", 4 * 1024 * 1024)
            require(time.time()-modified < 2)
            return json.loads(raw.decode("utf-8-sig"))
        # Existing probe publishes a whole JSON file by truncation/write. Await
        # only an absent/partial read; no command or input is repeated.
        return self.wait(sample, 3)

    def targets(self):
        return [self.target(i) for i in range(3)]

    def effects(self):
        return [{key: row[key] for key in ("native_event_count", "key_event_count", "mouse_event_count",
            "text_events", "wheel_events", "wheel_delta", "horizontal_wheel_events", "horizontal_wheel_delta", "held_keys", "buttons", "cursor")}
            for row in self.targets()]

    def input(self, action, **values):
        self.deadline()
        self.check = "command_"+action
        return self.command.tool("routing_input", {"action": action, **values})

    def source_users(self, rows):
        require(len(rows) == 2 and {u["name"] for u in rows} == {"Desktop A", "Desktop B"})
        require(all(u.get("desktop", {}).get("version") == 1 and u["desktop"]["provenance"] == "virtual_desktop_observation" for u in rows))
        self.identities(rows)
        return {u["name"]: u for u in rows}

    def native_rows(self):
        raw, _ = self.live_bytes("native_report", self.native_report, 4 * 1024 * 1024)
        raw = raw[:raw.rfind(b"\n")+1]
        rows = [json.loads(row) for row in raw.decode("utf-8-sig").splitlines()]
        require(not any(row["stage"] == "failed" for row in rows))
        return rows

    def native_stage(self, stage):
        values = [row["detail"] for row in self.native_rows() if row["stage"] == stage]
        require(len(values) <= 1)
        return values[0] if values else None

    def messages(self):
        return [json.loads(raw) for raw in json.loads(self.web("window.pimDesktop.messages()"))]

    def web_events(self):
        return [event for msg in self.messages() if msg.get("kind") == "event_stream" and msg["stream"]["kind"] == "events" for event in msg["stream"]["events"]]

    def native_events(self):
        return [row["detail"]["envelope"] for row in self.native_rows() if row["stage"] == "desktop_event"]

    def issue_desktop(self, slot):
        def published():
            new = self.descriptors()-self.issued
            require(len(new) <= 1)
            return next(iter(new)) if new else None
        path = self.wait(published)
        data = self.wait(lambda: self.strict_json("authority_descriptor", path))
        require(set(data) == {"protoinputmultiplex_application_events", "endpoint", "exact_origin", "credential", "expires_unix_ms", "desktop_observation_version"})
        require(data["protoinputmultiplex_application_events"] == data["desktop_observation_version"] == 1 and data["exact_origin"] == "http://127.0.0.1:4173")
        endpoint = urlsplit(data["endpoint"])
        require(endpoint.scheme == "ws" and endpoint.hostname == "127.0.0.1" and endpoint.port and endpoint.path == "/application-events/v1" and not (endpoint.query or endpoint.fragment or endpoint.username or endpoint.password))
        require(re.fullmatch(rf"protoinputmultiplex\.application-events\.{self.gui.pid}\.\d+\.\d+\.json",path.name) is not None)
        require(re.fullmatch("[0-9a-fA-F]{64}", data["credential"]) is not None)
        require(200000 < data["expires_unix_ms"]-time.time()*1000 <= 301000)
        self.paths[slot] = path; self.issued.add(path)
        return path

    def native_start(self, slot):
        descriptor = self.issue_desktop(slot)
        self.native_report = self.private / f"native-{slot}.jsonl"
        self.native = self.launch([self.native_exe, "--dll", self.dll, "--sha256", self.native_meta["dll"]["sha256"],
            "--descriptor", descriptor, "--report", self.native_report, "--desktop-route", "--observe-seconds", "60"], f"native-{slot}")
        self.native_runs.append(self.native)
        return self.wait(lambda: self.native_stage("desktop_baseline"), 20)

    def mark_native(self, slot, value):
        marker = self.paths[slot].with_suffix(".desktop-finish")
        with marker.open("xb") as output:
            output.write(value)

    def native_exit(self):
        require(self.native.wait(timeout=20) == 0)
        require(self.native_stage("desktop_observer_complete") and self.native_stage("dll_unloaded_before_owner_exit") and self.native_stage("owner_ready_to_exit_after_unload"))

    def web_connect(self, slot):
        path = self.issue_desktop(slot)
        descriptor = self.strict_json("authority_descriptor", path)
        self.web("window.pimClient.connect_desktop("+json.dumps(descriptor["endpoint"])+","+json.dumps(descriptor["credential"])+",1); true")
        return self.wait(self.state)

    def browser_start(self):
        edge = Path(os.environ["ProgramFiles(x86)"]) / "Microsoft/Edge/Application/msedge.exe"
        require(edge.is_file())
        profile = self.private / "edge-profile"
        self.browser = self.launch([edge, "--headless=new", "--window-size=1280,1000", "--disable-gpu", "--disable-extensions", "--no-first-run", "--no-default-browser-check", "--remote-debugging-address=127.0.0.1", "--remote-debugging-port=0", "--user-data-dir="+str(profile), "about:blank"], "edge")
        def devtools():
            raw, _ = self.live_bytes("browser_devtools", profile / "DevToolsActivePort", 4096)
            lines = raw.decode("utf-8-sig").splitlines()
            return lines if len(lines) == 2 and lines[0].isdigit() else None
        lines = self.wait(devtools)
        self.cdp = base.CDP("ws://127.0.0.1:"+lines[0]+lines[1], deadline=self.end)
        target = self.cdp.call("Target.createTarget", {"url": "http://127.0.0.1:4173/provider.html"})["targetId"]
        self.session = self.cdp.call("Target.attachToTarget", {"targetId": target, "flatten": True})["sessionId"]
        self.wait(lambda: self.web("!!window.pimClient && !!window.pimDesktop"))

    def assert_shared_ids(self):
        self.check = "paired_exact_baseline"
        native = self.source_users(self.wait(lambda: self.native_stage("desktop_baseline"))["users"])
        web = self.source_users(self.wait(self.state)["users"])
        require(self.identities(list(native.values())) == self.identities(list(web.values())))
        return native, web

    def phase(self, name):
        self.stage = name
        if name == "routing_ready":
            def ready():
                status = self.launch_status()
                return status if status["controller_present"] and status["ready"] and not status["preflight_pending"] and not status["stopping"] else None
            self.wait(ready)
            self.wait(lambda: all(row["frames"] > 0 for row in self.targets()))
        elif name == "command_grant":
            path = self.wait(lambda: next(iter(self.temp.glob(f"protoinputmultiplex-agent-{self.gui.pid}-*.json")), None))
            self.command_descriptor = path
            self.command = CommandMcp(self.exe, path, self.private / "command-mcp.log")
            def granted():
                state = self.command.tool("routing_state")
                policy = state["policy"]
                return state if set(policy["user_ids"]) == set(self.seat_ids) and set(policy["allowed_pids"]) == set(self.pids) and policy["session_control"] else None
            self.wait(granted)
        elif name == "prepare_holds":
            windows = self.command.tool("routing_windows")["windows"]
            for index, pid in enumerate(self.pids):
                rows = [row for row in windows if row["pid"] == pid and row.get("eligibility", {}).get("state") in ("fresh","reusable")]
                require(len(rows) == 1)
                row = rows[0]
                self.input("select_window", user=index, pid=pid, hwnd=row["hwnd"], creation_token=row["eligibility"]["creation_token"])
                self.input("move_to", user=index, x=self.points[index][0], y=self.points[index][1])
            self.input("button", user=0, button="primary", pressed=True)
            self.input("key", user=1, key="rightctrl", pressed=True)
            self.wait(lambda: self.target(0)["buttons"] & 8 and 163 in self.target(1)["held_keys"])
            require(any(e["vk"] == 163 and e["scan"] == 29 and e["extended"] is True and e["pressed"] is True for e in self.target(1)["key_events"]))
        elif name.startswith("bound_"):
            self.wait(lambda: self.bound(int(name[-1])))
        elif name == "native_one":
            self.native_start(1)
        elif name == "web_one":
            self.browser_start(); self.web_connect(2)
            native, web = self.assert_shared_ids(); self.first_ids = self.identities(list(web.values()))
            before = self.effects()
            for index,name in enumerate(("Desktop A","Desktop B")):
                position = {"x":self.points[index][0],"y":self.points[index][1]}
                require(native[name]["pointer"]["position"] == web[name]["pointer"]["position"] == position)
                require(before[index]["cursor"] == self.points[index])
            require(web["Desktop A"]["pointer"]["held_buttons"] == ["x1"])
            require(web["Desktop B"]["desktop"]["native_held_keys"] == [{"virtual_key": 163, "scan_code": 29, "prefix": "e0"}])
            require(native["Desktop A"]["pointer"] == web["Desktop A"]["pointer"] and native["Desktop B"]["desktop"]["native_held_keys"] == web["Desktop B"]["desktop"]["native_held_keys"])
            self.result["atomic_held_bootstrap"] = True
            self.result["native_wasm_exact_same_ids"] = True
            self.initial_users = web
            self.check = "observer_mutation_denial"
            commands = [{"kind": "rename_user", "user": web["Desktop A"]["id"], "name": "Forbidden"}, {"kind": "dispose_user", "user": web["Desktop A"]["id"]}, {"kind": "cursor", "user": web["Desktop A"]["id"], "control": {"kind": "color", "value": [1,2,3,255]}}, {"kind": "input", "user": web["Desktop A"]["id"], "input": {"kind": "motion", "position": {"x": self.points[0][0], "y": self.points[0][1]}}}]
            require(self.web("window.pimDesktop.deny("+json.dumps(json.dumps([json.dumps(c) for c in commands]))+")") == 4)
            require(self.wait(lambda: self.native_stage("desktop_observer_mutation_denied"))["pending_requests"] == 0)
            require(self.effects() == before)
            self.result["observer_mutation_denial_no_target_effect"] = True
            self.input("button", user=0, button="primary", pressed=False); self.input("key", user=1, key="rightctrl", pressed=False)
            self.produce_main()
        elif name == "web_text":
            self.web("window.pimClient.disconnect(); true"); self.web_connect(3)
            self.produce_text("Allowed café 🙂", 0, native_allowed=False)
            self.shared_prepare()
        elif name == "held_unbound":
            self.check = "held_unbind_no_target_effect"
            self.wait(lambda: self.bound(1))
            require(self.effects() == self.before_unbind)
            self.result["held_unbind_no_target_effect"] = True
        elif name == "shared_rebound":
            self.wait(lambda: self.bound(2))
            require(self.effects() == self.before_unbind)
        elif name == "revoke_native":
            self.check = "native_revocation_lifetime"
            self.native_exit()
            require(self.effects() == self.before_unbind)
            self.result["native_revoke_cleared_and_unloaded"] = True
        elif name == "native_two":
            self.native_start(4)
        elif name == "web_two":
            self.web("window.pimClient.disconnect(); true"); self.web_connect(5)
            native, web = self.assert_shared_ids(); self.second_ids = self.identities(list(web.values()))
            require(web["Desktop B"]["id"] == self.initial_users["Desktop B"]["id"] and web["Desktop A"]["id"] != self.initial_users["Desktop A"]["id"])
            for users in (native, web):
                require(all(any(key["virtual_key"] == 117 for key in u["desktop"]["native_held_keys"]) for u in users.values()))
            self.result["shared_key_held_bootstrap"] = True
            self.input("key", user=0, key="f6", pressed=False)
            require(117 in self.target(0)["held_keys"])
            self.input("key", user=1, key="f6", pressed=False)
            self.wait(lambda: 117 not in self.target(0)["held_keys"])
            require(self.target(0)["key_event_count"] == self.shared_key_count+2 and self.target(1)["key_event_count"] == self.shared_beta_count)
            self.command.tool("routing_mode", {"mode": "concurrent", "multi_keyboard": True})
            self.produce_text("Native 🙂", 1, native_allowed=True)
            self.input("key", user=0, key="rightctrl", pressed=True)
            self.input("button", user=1, button="primary", pressed=True)
            self.wait(lambda: 163 in self.target(0)["held_keys"] and self.target(1)["buttons"] & 1)
            self.mark_native(4, b"revoked\n")
        elif name == "stopped_one":
            self.check = "stop_cleanup_native_web_targets"
            self.wait(lambda: not self.launch_status()["controller_present"] and not self.launch_status()["stopping"])
            self.wait(lambda: all(not row["held_keys"] and row["buttons"] == 0 for row in self.targets()))
            self.native_exit(); self.command.close()
            self.wait(lambda: json.loads(self.web("window.pimClient.state_json()"))["users"] == [])
            self.web("window.pimClient.disconnect(); true")
            self.issued = set(); self.restart_effects = self.effects()
            self.result["stop_cleared_native_web_and_target_holds"] = True
        elif name == "web_restart":
            self.check = "restart_identity_and_no_replay"
            self.web_connect(6)
            current = self.source_users(self.wait(self.state)["users"])
            require(not (set(self.identities(list(current.values()))) & set(self.second_ids)))
            old = self.initial_users["Desktop A"]["id"]
            require(self.web("(()=>{try{window.pimClient.select_user("+json.dumps(json.dumps(old))+");return false;}catch(_){return true;}})()"))
            require(self.effects() == self.restart_effects)
            self.result["restart_fresh_ids_stale_denied_no_replay"] = True
        elif name == "stopped_two":
            self.wait(lambda: not self.launch_status()["controller_present"] and not self.launch_status()["stopping"])
            self.web("window.pimClient.disconnect(); true")
            require(self.effects() == self.restart_effects)
        else:
            raise base.Rejected()

    def produce_main(self):
        before = self.targets(); start_native = len(self.native_events()); start_web = len(self.web_events())
        for user in (0,1):
            self.input("motion", user=user, dx=6, dy=3)
            for pressed in (True,False): self.input("button", user=user, button="primary", pressed=pressed)
            self.input("wheel", user=user, delta=120, axis="vertical")
            self.input("wheel", user=user, delta=-120, axis="horizontal")
            for pressed in (True,False): self.input("key", user=user, key="f5", pressed=pressed)
        for pressed in (True,False): self.input("key", user=2, key="f8", pressed=pressed)
        def traces():
            rows = self.targets()
            for i in (0,1):
                row = rows[i]; native = row["native_events"][before[i]["native_event_count"]:]
                if row["wheel_delta"] != before[i]["wheel_delta"]+120 or row["horizontal_wheel_delta"] != before[i]["horizontal_wheel_delta"]-120: return None
                if row["cursor"] != [self.points[i][0]+6,self.points[i][1]+3] or row["buttons"] != 0 or row["held_keys"]: return None
                for kind,code in (("key",116),("button",3 if i == 0 else 0)):
                    edges = [e["pressed"] for e in native if e["kind"] == kind and e["code"] == code]
                    if not any(edges[j:j+2] == [True,False] for j in range(len(edges)-1)): return None
                if not any(e["kind"] == "motion" for e in native): return None
                for kind,delta in (("wheel",120),("horizontal_wheel",-120)):
                    if not any(e["kind"] == kind and e["delta"] == delta and e["wheel_screen"] == [self.points[i][0]+6,self.points[i][1]+3] for e in native): return None
            third = rows[2]["native_events"][before[2]["native_event_count"]:]
            if rows[2]["key_event_count"] != before[2]["key_event_count"]+2 or rows[2]["held_keys"]: return None
            if [e["pressed"] for e in third if e["kind"] == "key" and e["code"] == 119] != [True,False]: return None
            return rows
        self.check = "independent_target_traces"
        self.wait(traces)
        def routed_records():
            for observed in (self.native_events()[start_native:],self.web_events()[start_web:]):
                for index,name in enumerate(("Desktop A","Desktop B")):
                    user = self.initial_users[name]["id"]
                    changes = [e["event"]["record"]["change"] for e in observed if e["event"]["kind"] == "desktop_observed" and e["event"]["user"] == user]
                    wanted = [{"kind":"motion","position":{"x":self.points[index][0]+6,"y":self.points[index][1]+3}},
                        {"kind":"wheel","axis":"vertical","units":120},{"kind":"wheel","axis":"horizontal","units":-120}]
                    for transition in ("down","up"):
                        wanted.append({"kind":"button","button":"x1" if index == 0 else "primary","transition":transition})
                        wanted.append({"kind":"key","key":{"virtual_key":116,"scan_code":63,"prefix":"none"},"transition":transition})
                    if not all(change in changes for change in wanted): return None
            return True
        self.check = "native_and_wasm_record_families"
        self.wait(routed_records)
        require(all(e["event"]["user"] in [u["id"] for u in self.initial_users.values()] for e in self.native_events()+self.web_events() if e["event"]["kind"] == "desktop_observed"))
        self.result["independent_two_target_traces_and_third_exclusion"] = True
        self.produce_text("Hidden 🙂", 0, native_allowed=False, web_allowed=False)
        # One fixed ordinary command Batch gives Down/Up then later Motion. No replay.
        point = self.points[0]; self.input("move_to", user=0, x=point[0], y=point[1])
        screen = self.wait(self.state)["screens"]["desktop"]
        require(self.web("window.pimDesktop.installClick("+json.dumps(json.dumps(screen))+","+json.dumps(json.dumps({"x":point[0],"y":point[1]}))+")"))
        batch_before = self.target(0)
        one_owned_click_batch(self.command_descriptor)
        self.check = "rust_wasm_virtual_selector_callback"
        self.wait(lambda: len(json.loads(self.web("window.pimDesktop.clicks()"))) == 1)
        click = json.loads(json.loads(self.web("window.pimDesktop.clicks()"))[0])
        clicked = click["event"]; event = clicked["event"]
        expected = self.initial_users["Desktop A"]
        require(click["kind"] == "virtual_click" and click["source"] == "authorized_virtual_stream" and click["selector"] == "#provider-hit")
        require(event["kind"] == "desktop_observed" and event["user"] == expected["id"] and event["source"] == expected["source"])
        require(event["record"]["change"] == {"kind":"button","button":"x1","transition":"up"} and event["position"] == {"x":point[0],"y":point[1]})
        def later_motion(envelope):
            value = envelope["event"]
            return envelope["epoch"] == clicked["epoch"] and envelope["sequence"] > clicked["sequence"] and value["kind"] == "desktop_observed" and value["user"] == event["user"] and value["source"] == event["source"] and value["record"]["change"]["kind"] == "motion" and value["position"] == {"x":point[0]+32,"y":point[1]}
        def historical_callback():
            if clicked not in self.web_events() or clicked not in self.native_events(): return None
            if not any(later_motion(e) for e in self.web_events()) or not any(later_motion(e) for e in self.native_events()): return None
            row = self.target(0)
            edges = [e["pressed"] for e in row["native_events"][batch_before["native_event_count"]:] if e["kind"] == "button" and e["code"] == 3]
            return row["cursor"] == [point[0]+32,point[1]] and row["buttons"] == 0 and edges == [True,False]
        self.check = "selector_actual_historical_callback_and_target"
        self.wait(historical_callback)
        # Transport grouping is incidental. A controlled same-batch unit test
        # proves the old-position versus final-position branch separately.
        self.result["wasm_selector_later_motion_same_web_batch"] = any(
            clicked in msg["stream"]["events"] and any(later_motion(e) for e in msg["stream"]["events"])
            for msg in self.messages() if msg.get("kind") == "event_stream" and msg["stream"]["kind"] == "events")
        self.result["wasm_selector_uses_historical_event_point"] = True

    def produce_text(self, text, user, native_allowed, web_allowed=True):
        native_before = len(self.native_events()); web_before = len(self.web_events()); target_before = self.target(user)
        self.input("text", user=user, text=text)
        self.wait(lambda: self.target(user)["text"] == target_before["text"]+text)
        units = list(struct.unpack("<"+"H"*(len(text.encode("utf-16-le"))//2), text.encode("utf-16-le")))
        target_units = [e["code"] for e in self.target(user)["native_events"][target_before["native_event_count"]:] if e["kind"] == "char"]
        require(target_units == units)
        def observed():
            native = [e["event"]["record"]["change"]["unit"] for e in self.native_events()[native_before:] if e["event"]["record"]["change"]["kind"] == "text_unit"]
            web = [e["event"]["record"]["change"]["unit"] for e in self.web_events()[web_before:] if e["event"]["kind"] == "desktop_observed" and e["event"]["record"]["change"]["kind"] == "text_unit"]
            require(native == units[:len(native)] if native_allowed else native == [])
            require(web == units[:len(web)] if web_allowed else web == [])
            return native == (units if native_allowed else []) and web == (units if web_allowed else [])
        self.check = "utf16_projection_and_consent"
        self.wait(observed)
        # A marker F5 edge fences consent-negative assertions behind processed events.
        marker_before = self.target(user)
        for pressed in (True,False): self.input("key", user=user, key="f5", pressed=pressed)
        def marker_seen():
            for observed in (self.native_events()[native_before:],self.web_events()[web_before:]):
                if not any(e["event"]["kind"] == "desktop_observed" and e["event"]["record"]["change"].get("key",{}).get("virtual_key") == 116 and e["event"]["record"]["change"].get("transition") == "up" for e in observed): return None
            return True
        self.wait(marker_seen)
        def target_marker_seen():
            row = self.target(user)
            edges = [e["pressed"] for e in row["native_events"][marker_before["native_event_count"]:] if e["kind"] == "key" and e["code"] == 116]
            return row["key_event_count"] == marker_before["key_event_count"]+2 and 116 not in row["held_keys"] and edges == [True,False]
        self.wait(target_marker_seen)
        self.check = "utf16_after_native_web_marker"
        require(observed())
        self.result["text_consent_native_"+str(native_allowed).lower()+"_web_"+str(web_allowed).lower()] = True

    def shared_prepare(self):
        self.check = "shared_keyboard_mode"
        self.command.tool("routing_mode", {"mode":"concurrent","multi_keyboard":False})
        self.input("move_to", user=0, x=self.points[0][0], y=self.points[0][1])
        for pressed in (True,False): self.input("button", user=0, button="primary", pressed=pressed)
        before = self.target(0)["key_event_count"]
        self.shared_key_count, self.shared_beta_count = before, self.target(1)["key_event_count"]
        self.input("key", user=0, key="f6", pressed=True); self.input("key", user=1, key="f6", pressed=True)
        self.wait(lambda: 117 in self.target(0)["held_keys"])
        self.check = "shared_native_down_suppression"
        require(self.target(0)["key_event_count"] == before+1)
        def shared_record():
            user = self.initial_users["Desktop B"]["id"]
            for observed in (self.native_events(),self.web_events()):
                if not any(e["event"]["kind"] == "desktop_observed" and e["event"]["user"] == user and e["event"]["record"]["admission"] == "shared_native_hold" and e["event"]["record"]["change"].get("key",{}).get("virtual_key") == 117 and e["event"]["record"]["change"].get("transition") == "down" for e in observed): return None
            return True
        self.wait(shared_record)
        self.before_unbind = self.effects(); self.mark_native(1, b"revoked\n")

    def steps(self):
        out = []
        def add(label, phase=None, fields=None):
            step = {"button":label,"wait_ms":250,"whole_response":True}
            if fields: step["fields"] = fields
            if phase: step["wait_for_sample"] = True; self.phase_steps[len(out)+1] = phase
            out.append(step)
        def sdk():
            for label in ("Settings","Settings category Agent access","Agent Access","SDK tab","Application event SDK section"): add(label)
        def masters():
            add("Master pointer observation"); add("Master keyboard observation separately"); add("Apply master event consent")
        def receiver(text=False):
            add("Add event receiver")
            add("Select all visible users", fields={"application_event_origin":"http://127.0.0.1:4173","application_event_ttl":"300"})
            for label in ("Approve pointer event observation","Approve keyboard event observation separately","Approve routed desktop observations"): add(label)
            if text: add("Include typed desktop text separately")
            add("Apply selected event receiver approvals")
        add("Start routing","routing_ready")
        for label in ("Settings","Settings category Agent access","Agent Access"): add(label)
        add("Enable agent access", fields={"agent_users":','.join(self.seat_ids),"agent_pids":','.join(map(str,self.pids)),"agent_session_control":"false","agent_capture":"false","agent_app_preferences":"false"})
        add("Additional permissions"); add("Agent session control permission"); add("Apply access policy","command_grant")
        add("SDK tab"); add("Application event SDK section"); add("Create cooperative event session","prepare_holds"); masters()
        add("Observe desktop user 1: Desktop A","bound_1"); add("Observe desktop user 2: Desktop B","bound_2")
        # Receiver rows/origins are authored before listener start; Add is then unavailable.
        receiver(); receiver(); add("Event receiver 1 · approved"); add("Start application event listener"); add("Issue private event-only connection","native_one")
        add("Event receiver 2 · approved"); add("Issue private event-only connection","web_one")
        add("Include typed desktop text separately"); add("Apply selected event receiver approvals"); add("Issue private event-only connection","web_text")
        add("Stop observing desktop user 1: Desktop A","held_unbound"); add("Observe desktop user 1: Desktop A","shared_rebound")
        add("Event receiver 1 · approved"); add("Revoke selected event receiver","revoke_native")
        add("Select all visible users"); add("Include typed desktop text separately"); add("Apply selected event receiver approvals"); add("Issue private event-only connection","native_two")
        add("Event receiver 2 · approved"); add("Select all visible users"); add("Apply selected event receiver approvals"); add("Issue private event-only connection","web_two")
        add("Back to Home"); add("Stop routing","stopped_one"); add("Start routing","routing_ready"); sdk()
        add("Create cooperative event session"); masters(); add("Observe desktop user 1: Desktop A","bound_1"); add("Observe desktop user 2: Desktop B","bound_2")
        receiver(text=True); add("Start application event listener"); add("Issue private event-only connection","web_restart")
        add("Back to Home"); add("Stop routing","stopped_two")
        return out

    def run(self):
        args = self.args
        require(os.name == "nt" and os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted")
        require(not self.private.exists()); self.private.mkdir()
        self.ui, self.temp = self.private/"ui", self.private/"temp"; self.ui.mkdir(); self.temp.mkdir()
        gui_root = args.artifacts/"sdk-preview/gui"; native_root = args.artifacts/"sdk-preview/x64"
        gui_meta = self.strict_json("artifact_json",gui_root/"metadata.json"); self.native_meta = self.strict_json("artifact_json",native_root/"metadata.json")
        require(gui_meta["source_commit"] == self.native_meta["source_commit"] == args.commit and gui_meta["features"] == ["software-renderer"] and self.native_meta["native_client_abi_version"] == 3)
        self.exe = self.checked(gui_root,gui_meta["executable"],"protoinputmultiplex.exe")
        self.native_exe = self.checked(native_root,self.native_meta["external_fixture"],"native_external_consumer.exe")
        self.dll = self.checked(native_root,self.native_meta["dll"],"mux_application_sdk.dll")
        backend = base.read(args.backend/"provider-backend.json")
        require(backend["source_commit"] == args.commit and backend["backend_pin"] == "dd29bf8d22349605d0e1950c1c5dc4893d72670e" and backend["architecture"] == "x64")
        (self.private/"runtime").mkdir()
        for entry in backend["files"]:
            require(re.fullmatch(r"[A-Za-z0-9_.-]+\.(dll|exe)",entry["file"]) is not None)
            source = self.checked(args.backend,entry,entry["file"]); shutil.copyfile(source,self.private/"runtime"/entry["file"])
        prefs = self.private/"preferences"; prefs.mkdir(); self.preferences = prefs/"app-preferences.json"
        data = self.strict_json("preferences_json",args.source/"fixtures/gui-sdk-preparation/app-preferences.json")
        data["load_profile_at_startup"] = True
        self.preferences.write_text(json.dumps(data),encoding="utf-8"); self.preferences_initial = self.preferences.read_bytes()
        self.targets_processes = [self.launch([self.exe,"probe","--automation","--report",self.private/f"target-{i}.json"],f"target-{i}") for i in range(3)]
        self.pids = [p.pid for p in self.targets_processes]
        self.place_targets()
        self.seat_ids = ["provider-a","provider-b","provider-c"]
        profile = {"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":self.pids},"seats":[{"id":self.seat_ids[i],"name":"Desktop "+chr(65+i),"mouse":f"virtual://mouse/{i+1}","keyboard":f"virtual://keyboard/{i+1}","target_pid":self.pids[i],"desktop_start":self.points[i],"cursor":{"lock_window":True,"horizontal_wheel":"preserve",**({"button_mapping":["x1","secondary","middle","primary","x2"]} if i == 0 else {})}} for i in range(3)]}
        self.profile = self.private/"profile.json"; self.profile.write_text(json.dumps(profile),encoding="utf-8"); self.profile_initial = self.profile.read_bytes()
        self.phase_steps = {}; steps = self.steps(); (self.ui/"prepare.json").write_text(json.dumps(steps),encoding="utf-8")
        self.serve_web()
        env = dict(os.environ,TEMP=str(self.temp),TMP=str(self.temp),PROTOINPUTMULTIPLEX_CONTROL_BACKEND="native")
        self.gui = self.launch([self.exe,"gui","--renderer","software-dx12","--profile",self.profile,"--preferences",self.preferences,"--ui-script",self.ui/"prepare.json","--ui-script-no-screenshots"],"gui",env)
        for index,step in enumerate(steps,1):
            self.checkpoint = index; self.stage = "ui_receipt"; self.check = "none"
            self.wait(lambda: len(self.receipts()) >= index)
            row = self.receipts()[index-1]; require(row["button"] == step["button"] and row["clicked"] is True)
            if index in self.phase_steps:
                # Actions execute exactly once. Only each phase's observations wait.
                self.phase(self.phase_steps[index]); self.acknowledge(index)
        self.deadline(); self.cdp.call("Browser.close"); require(self.browser.wait(timeout=12) == 0)
        self.check = "ordinary_owned_process_closure"
        self.http.shutdown(); self.http.server_close()
        self.deadline(); self.close_gui_owned_main(); require(self.gui.wait(timeout=20) == 0)
        for hwnd in self.target_windows:
            self.deadline(); require(self.user32.PostMessageW(hwnd,0x0010,0,0))
        require(all(p.wait(timeout=12) == 0 for p in self.targets_processes))
        require(self.profile.read_bytes() == self.profile_initial and self.preferences.read_bytes() == self.preferences_initial)
        require(not self.descriptors() and not list(self.temp.glob(f"protoinputmultiplex-agent-{self.gui.pid}-*.json")))
        mandatory = ("atomic_held_bootstrap","native_wasm_exact_same_ids","observer_mutation_denial_no_target_effect",
            "independent_two_target_traces_and_third_exclusion","wasm_selector_uses_historical_event_point",
            "text_consent_native_false_web_false","text_consent_native_false_web_true","text_consent_native_true_web_true",
            "held_unbind_no_target_effect","native_revoke_cleared_and_unloaded","shared_key_held_bootstrap",
            "stop_cleared_native_web_and_target_holds","restart_fresh_ids_stale_denied_no_replay")
        require(all(self.result.get(flag) is True for flag in mandatory) and len(self.native_runs) == 2)
        self.deadline()
        self.result.update(status="PASS",native_exit_codes=[p.returncode for p in self.native_runs],gui_exit_code=0,target_exit_codes=[p.returncode for p in self.targets_processes],ordinary_cleanup=True,profile_and_preferences_unchanged=True)

    def place_targets(self):
        user = ctypes.WinDLL("user32",use_last_error=True); self.user32 = user
        user.SetThreadDpiAwarenessContext.argtypes = [ctypes.c_void_p]; user.SetThreadDpiAwarenessContext.restype = ctypes.c_void_p
        require(user.SetThreadDpiAwarenessContext(ctypes.c_void_p(-4)))
        callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL,wintypes.HWND,wintypes.LPARAM)
        user.EnumWindows.argtypes = [callback_type,wintypes.LPARAM]
        user.GetWindowThreadProcessId.argtypes = [wintypes.HWND,ctypes.POINTER(wintypes.DWORD)]
        user.GetWindow.argtypes = [wintypes.HWND,wintypes.UINT]; user.GetWindow.restype = wintypes.HWND
        user.SetWindowPos.argtypes = [wintypes.HWND,wintypes.HWND,ctypes.c_int,ctypes.c_int,ctypes.c_int,ctypes.c_int,wintypes.UINT]
        user.ClientToScreen.argtypes = [wintypes.HWND,ctypes.POINTER(wintypes.POINT)]
        user.PostMessageW.argtypes = [wintypes.HWND,wintypes.UINT,wintypes.WPARAM,wintypes.LPARAM]
        def found():
            windows = {}
            @callback_type
            def visit(hwnd,_):
                pid = wintypes.DWORD(); user.GetWindowThreadProcessId(hwnd,ctypes.byref(pid))
                if pid.value in self.pids and not user.GetWindow(hwnd,4): windows[pid.value] = hwnd
                return True
            require(user.EnumWindows(visit,0)); return windows if set(windows) == set(self.pids) else None
        windows = self.wait(found); self.target_windows = [windows[pid] for pid in self.pids]; self.points=[]
        left,top,width,height = [user.GetSystemMetrics(index) for index in (76,77,78,79)]
        require(width >= 640 and height >= 480)
        target_width = min(360,(width-40)//3-10); target_height = min(320,height-60)
        for index,hwnd in enumerate(self.target_windows):
            require(user.SetWindowPos(hwnd,wintypes.HWND(-1),left+10+(target_width+10)*index,top+30,target_width,target_height,0x10))
            point=wintypes.POINT(140,120); require(user.ClientToScreen(hwnd,ctypes.byref(point))); self.points.append([point.x,point.y])

    def serve_web(self):
        root=self.args.artifacts/"application-web"; meta=self.strict_json("artifact_json",root/"metadata.json")
        require(meta["source_commit"] == self.args.commit and meta["rust_wasm"] is True)
        public=self.private/"static"; public.mkdir(); (public/"pkg").mkdir()
        require({entry["file"] for entry in meta["files"]} == {"index.html","bootstrap.js","pkg/mux_application_web.js","pkg/mux_application_web_bg.wasm"})
        for entry in meta["files"]: shutil.copyfile(self.checked(root,entry,entry["file"]),public/entry["file"])
        shutil.copyfile(self.args.source/"crates/mux-application-web/example/desktop_provider_acceptance.js",public/"desktop_provider_acceptance.js")
        (public/"provider.js").write_text("import {client} from './bootstrap.js'; import {installDesktopAcceptance} from './desktop_provider_acceptance.js'; window.pimClient=client; window.pimDesktop=installDesktopAcceptance(client);",encoding="utf-8")
        (public/"provider.html").write_text('<!doctype html><main id="application-demo"></main><script type="module" src="./provider.js"></script>',encoding="utf-8")
        class Handler(http.server.SimpleHTTPRequestHandler):
            def __init__(self,*values,**kwargs): super().__init__(*values,directory=str(public),**kwargs)
            def log_message(self,*values): pass
        self.http=base.StaticServer(("127.0.0.1",4173),Handler); threading.Thread(target=self.http.serve_forever,daemon=True).start()


def main():
    parser=argparse.ArgumentParser()
    for name in ("artifacts","backend","source","run","public-result"): parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--commit",required=True); args=parser.parse_args(); gate=ProviderPair(args)
    try:
        gate.run()
    except BaseException as error:
        allowed={"Rejected","AssertionError","FileNotFoundError","TimeoutError","BrokenPipeError","EOFError","JSONDecodeError"}
        gate.result.update(status="FAIL",checkpoint=gate.checkpoint,failure_stage=gate.stage,failure_predicate=gate.check,failure_class=type(error).__name__ if type(error).__name__ in allowed else "UnexpectedError",
            native_exit_state="not_started" if not hasattr(gate,"native") else "running" if gate.native.poll() is None else "zero" if gate.native.returncode == 0 else "nonzero")
    require(not args.public_result.exists())
    args.public_result.write_text(json.dumps(gate.result,indent=2)+"\n",encoding="utf-8")
    raise SystemExit(0 if gate.result["status"] == "PASS" else 1)


if __name__ == "__main__":
    main()
