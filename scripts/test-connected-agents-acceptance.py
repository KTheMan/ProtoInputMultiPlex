"""Independent GUI-connected agent gate preparation. No launch before READY.

The public pipe framing/authentication helpers below operate only on the private
descriptor created by our disposable GUI. They never publish its bearer token
or pipe name. The integrated GUI/MCP runner is aligned after contracts settle.
"""
import argparse
import base64
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import queue
import struct
import subprocess
import tempfile
import threading
import time
from PIL import Image

_spec = importlib.util.spec_from_file_location("connected_ui_helpers", Path(__file__).with_name("test-replication-graph-ui.py"))
_ui = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_ui)
eventually, user32 = _ui.eventually, _ui.user32
Rect = _ui._native._studio._base._client_module.Rect
user32.GetClientRect.argtypes = [wintypes.HWND, ctypes.POINTER(Rect)]

kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
kernel32.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
kernel32.CreateFileW.restype = wintypes.HANDLE
kernel32.SetNamedPipeHandleState.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p, ctypes.c_void_p]
kernel32.ReadFile.argtypes = [wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p]
kernel32.WriteFile.argtypes = kernel32.ReadFile.argtypes
kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
kernel32.LocalFree.argtypes = [ctypes.c_void_p]
advapi32.GetSecurityInfo.argtypes = [wintypes.HANDLE, ctypes.c_int, wintypes.DWORD, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p)]
advapi32.GetNamedSecurityInfoW.argtypes = [wintypes.LPWSTR, ctypes.c_int, wintypes.DWORD, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p)]
advapi32.ConvertSecurityDescriptorToStringSecurityDescriptorW.argtypes = [ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, ctypes.POINTER(wintypes.LPWSTR), ctypes.POINTER(wintypes.DWORD)]
advapi32.GetSecurityDescriptorControl.argtypes = [ctypes.c_void_p, ctypes.POINTER(wintypes.WORD), ctypes.POINTER(wintypes.DWORD)]

MAX_FRAME = 16 * 1024 * 1024
INVALID_HANDLE = ctypes.c_void_p(-1).value

MATRIX = {
    "authentication_and_tags": "Protected actual logon-SID descriptor and pipe ACL, wrong-token rejection, two authenticated client IDs with overlapping sequence numbers and matching command ACKs.",
    "ownership": "GUI manual F5 plus two agents holding F5: releasing/disconnecting agents preserves the GUI hold; EOF releases agent-only F7 and retains the running GUI session and other seat's manual F6.",
    "scope_and_revocation": "Disallowed user, explicitly excluded owned PID, capture permission and session-control requests reject without mutating native holds or GUI policy. Actual GUI Disarm revokes agent ownership and subsequent input while originals remain live.",
    "capture": "Capture permission enabled through actual controls yields a decoded PNG of the exact selected owned patterned child. Work is asynchronous: native input to another owned seat proceeds during a real large selected-target capture/PNG job.",
    "gui_lifecycle": "Actual Agent Access arm/disarm/capture controls, connected MCP operations and tagged ACKs; operator Stop destroys owned overlays and removes descriptor/listener; no new caller-owned routing session replaces the GUI session."
}


def security_metadata(descriptor_path=None, handle=None):
    """Query real DACL; only its non-secret properties/hash enter evidence."""
    sd = ctypes.c_void_p()
    if descriptor_path is not None:
        result = advapi32.GetNamedSecurityInfoW(str(descriptor_path), 1, 4, None, None, None, None, ctypes.byref(sd))
        object_type = "file"
    else:
        # Both are legitimate Windows object categories. Record which accepted
        # the actual owned pipe handle; no fabricated ACL or fallback assertion.
        result = advapi32.GetSecurityInfo(handle, 6, 4, None, None, None, None, ctypes.byref(sd))
        object_type = "kernel"
        if result:
            result = advapi32.GetSecurityInfo(handle, 1, 4, None, None, None, None, ctypes.byref(sd))
            object_type = "file"
    assert result == 0 and sd.value, ("Query owned security descriptor", result)
    text = wintypes.LPWSTR()
    try:
        control, revision = wintypes.WORD(), wintypes.DWORD()
        assert advapi32.GetSecurityDescriptorControl(sd, ctypes.byref(control), ctypes.byref(revision))
        assert advapi32.ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, 1, 4, ctypes.byref(text), None)
        sddl = text.value
        aces = [part.split(")", 1)[0].split(";") for part in sddl.split("(")[1:]]
        assert control.value & 0x1000, "DACL is not protected against inheritance"
        assert len(aces) == 1 and len(aces[0]) == 6, "Unexpected additional ACL principals"
        ace = aces[0]
        assert ace[0] == "A" and ace[2] in ("GA", "0x10000000", "FA", "0x1f01ff")
        assert ace[5].startswith("S-1-5-5-"), "ACL principal is not the actual logon SID"
        return {"protected": True, "ace_count": 1, "principal": "current_logon_sid", "full_access": True, "object_type": object_type, "sddl_sha256": hashlib.sha256(sddl.encode()).hexdigest()}
    finally:
        if text:
            kernel32.LocalFree(ctypes.cast(text, ctypes.c_void_p))
        kernel32.LocalFree(sd)


class OwnedPipe:
    """Finite raw public-protocol client for an OWN GUI-issued descriptor."""
    def __init__(self, descriptor_path, wrong_token=False):
        descriptor = json.loads(Path(descriptor_path).read_text(encoding="utf-8"))
        assert descriptor["pipe"].startswith("\\\\.\\pipe\\ProtoInputMultiPlex.")
        self.handle, self.client, self.sequence = None, None, 1
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            handle = kernel32.CreateFileW(descriptor["pipe"], 0xC0020000, 0, None, 3, 0, None)
            if handle != INVALID_HANDLE:
                self.handle = handle
                break
            error = ctypes.get_last_error()
            assert error in (2, 231), ("Open only owned GUI pipe", error)
            time.sleep(.01)
        assert self.handle, "Owned GUI pipe did not become available"
        try:
            mode = wintypes.DWORD(1)
            assert kernel32.SetNamedPipeHandleState(self.handle, ctypes.byref(mode), None, None)
            self.security = security_metadata(handle=self.handle)
            token = descriptor["token"]
            if wrong_token:
                token = ("1" if token[0] != "1" else "0") + token[1:]
            self.send({"type": "connect", "token": token})
            if wrong_token:
                try:
                    self.receive()
                except EOFError:
                    self.auth_rejected = True
                else:
                    raise AssertionError("Wrong bearer token was accepted")
                return
            reply = self.receive()["reply"]
            assert reply["acknowledged"] and reply["client"] > 0 and reply["sequence"] == 0 and not reply["error"]
            self.client = reply["client"]
        except Exception:
            self.close()
            raise

    def send(self, value):
        payload = json.dumps(value, ensure_ascii=False).encode("utf-8")
        assert len(payload) <= MAX_FRAME
        data = struct.pack(">I", len(payload)) + payload
        position, deadline = 0, time.monotonic() + 15
        while position < len(data) and time.monotonic() < deadline:
            chunk = ctypes.create_string_buffer(data[position:])
            written = wintypes.DWORD()
            if kernel32.WriteFile(self.handle, chunk, len(data) - position, ctypes.byref(written), None):
                position += written.value
            else:
                error = ctypes.get_last_error()
                if error in (109, 233):
                    raise EOFError("Owned GUI pipe closed")
                assert error in (232, 231), ("Write owned pipe frame", error)
            if position < len(data):
                time.sleep(.005)
        assert position == len(data), "Finite owned pipe write deadline"

    def read_exact(self, count):
        value, deadline = bytearray(), time.monotonic() + 15
        while len(value) < count and time.monotonic() < deadline:
            buffer = ctypes.create_string_buffer(min(65536, count - len(value)))
            read = wintypes.DWORD()
            if kernel32.ReadFile(self.handle, buffer, len(buffer), ctypes.byref(read), None):
                if read.value:
                    value.extend(buffer.raw[:read.value])
            else:
                error = ctypes.get_last_error()
                if error in (109, 233):
                    raise EOFError("Owned GUI pipe closed")
                assert error == 232, ("Read owned pipe frame", error)
            if len(value) < count:
                time.sleep(.005)
        assert len(value) == count, f"Finite owned pipe read deadline ({len(value)}/{count} bytes)"
        return bytes(value)

    def receive(self):
        size = struct.unpack(">I", self.read_exact(4))[0]
        assert 0 < size <= MAX_FRAME
        return json.loads(self.read_exact(size))

    def request(self, kind, **fields):
        sequence = self.sequence
        self.sequence += 1
        self.send({"type": kind, "sequence": sequence, **fields})
        reply = self.receive()["reply"]
        assert reply["acknowledged"] and reply["client"] == self.client and reply["sequence"] == sequence, "Actual tagged ACK mismatch"
        return reply

    def close(self):
        if self.handle:
            kernel32.CloseHandle(self.handle)
            self.handle = None


class ConnectedMcp(_ui.Client):
    def __init__(self, executable, descriptor, log):
        self.stderr = open(log, "w", encoding="utf-8")
        self.process = subprocess.Popen([str(executable), "mcp", "--gui-connection", str(descriptor)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr, text=True, encoding="utf-8", creationflags=subprocess.CREATE_NO_WINDOW)
        self.messages, self.next_id, self.latest = queue.Queue(), 0, False
        threading.Thread(target=self.read, daemon=True).start()
        try:
            result = self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "owned-connected-acceptance", "version": "1"}})
            assert result["capabilities"]["tools"] is not None
            self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        except Exception:
            self.process.kill()
            self.process.wait(timeout=10)
            self.stderr.close()
            raise


class ConnectedGate(_ui.UiGate):
    colors = [0x286098, 0x9b3456, 0x45876a, 0xb27718]
    expected_tools = _ui._native.GraphGate.tool_names | {"virtual_users", "agent_session", "routing_capture"}

    def __init__(self, args):
        super().__init__(args, "connected-agents", count=4)
        self.connections = []
        self.clients = []
        self.once = set()
        self.ids = ["connected-own-a", "connected-own-b", "connected-own-c"]
        self.descriptor = None
        self.pipe_name = None  # Secret-bearing name stays in memory, never evidence.

    def fixtures(self):
        for i in range(self.count):
            self.owned.append(subprocess.Popen([str(self.images[i % 2]), "probe", "--automation", "--report", str(self.run / f"probe-{i}.json"), "--capture-pattern", str(self.colors[i])], creationflags=subprocess.CREATE_NO_WINDOW))
        self.pids = [p.pid for p in self.owned]
        client = _ui.Client(self.images[0], self.args.runtime.resolve(), self.run / "discovery.stderr.log")
        try:
            tools = client.rpc("tools/list", {})["tools"]
            assert len(tools) == 20 and {t["name"] for t in tools} == self.expected_tools
            assert all(t["inputSchema"]["type"] == "object" for t in tools)
            self.evidence["tools"] = tools
            inventory = eventually(lambda: client.tool("routing_windows"), lambda v: all(any(w["pid"] == p and w["title"].startswith(_ui._native._studio._base.PREFIX) for w in v["windows"]) for p in self.pids), 60)
            for i, pid in enumerate(self.pids):
                window = next(w for w in inventory["windows"] if w["pid"] == pid and w["title"].startswith(_ui._native._studio._base.PREFIX))
                assert user32.SetWindowPos(window["hwnd"], wintypes.HWND(-1), 30 + i * 460, 40, 420, 360, 0x10)
            inventory = client.tool("routing_windows")
            self.windows = [next(w for w in inventory["windows"] if w["pid"] == pid and w["title"].startswith(_ui._native._studio._base.PREFIX)) for pid in self.pids]
            self.points = [[w["client"]["left"] + 120, w["client"]["top"] + 100] for w in self.windows]
        finally:
            client.close()

    def find_descriptor(self):
        pid = self.owned[-1].pid
        files = list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{pid}-*.json"))
        assert len(files) == 1, "Expected exactly one descriptor for this owned GUI PID"
        self.descriptor = files[0]
        self.pipe_name = json.loads(self.descriptor.read_text())["pipe"]
        return self.descriptor

    def probes(self):
        # Each own GUI fixture writes periodically. Retry a partial JSON read
        # here, rather than restarting an entire IPC phase in UiGate.execute.
        return eventually(lambda: super(ConnectedGate, self).probes(), lambda reports: True, 3)

    def raw(self):
        client = OwnedPipe(self.descriptor)
        self.connections.append(client)
        return client

    def mcp(self):
        client = ConnectedMcp(self.images[0], self.descriptor, self.run / f"connected-{len(self.clients)}.stderr.log")
        self.clients.append(client)
        return client

    def key_request(self, user, vk, down):
        # Original virtual devices are explicitly profile IDs 1/2/3.
        scans = {116: 63, 117: 64, 118: 65, 119: 66, 120: 67}
        return {"Input": {"device": 0xf1000003 + user * 2, "kind": 1, "flags": 0 if down else 1, "x": scans[vk], "y": 0, "data": vk}}

    def original_holds(self):
        rs = self.probes()
        assert 116 in rs[0]["held_keys"] and 117 in rs[1]["held_keys"] and not rs[2]["key_event_count"]
        return rs

    def first_connection_phase(self):
        self.find_descriptor()
        acl = security_metadata(descriptor_path=self.descriptor)
        wrong = OwnedPipe(self.descriptor, wrong_token=True)
        try:
            assert wrong.auth_rejected
        finally:
            wrong.close()
        first, second = self.raw(), self.raw()
        assert first.client != second.client
        self.evidence["connection_auth_prefix"] = {"descriptor_acl": acl, "pipe_acl": first.security, "wrong_token_rejected": True, "authenticated_client_ids": [first.client, second.client], "connect_sequence": 0}
        self.save()
        before = self.probes()
        tagged = [c.request("execute", request=self.key_request(0, 116, True)) for c in (first, second)]
        assert all(not v["error"] and v["sequence"] == 1 for v in tagged)
        first.request("execute", request="ReleaseAll")
        self.original_holds()
        second.close()
        eventually(self.probes, lambda rs: 116 in rs[0]["held_keys"] and 117 in rs[1]["held_keys"])
        assert self.probes()[0]["key_event_count"] == before[0]["key_event_count"], "Agent same-key claim generated an extra native edge"
        rejected = []
        # PID scope limits the actual native recipient, not overlay coordinates.
        # Existing focus A can receive motion while the overlay hovers over D.
        hover = first.request("execute", request={"MoveTo": {"user": 0, "x": self.points[3][0], "y": self.points[3][1]}})
        assert not hover["error"]
        assert self.probes()[3]["native_event_count"] == before[3]["native_event_count"]
        blocked_button = first.request("execute", request={"Input": {"device": 0xf1000002, "kind": 0, "flags": 1, "x": 0, "y": 0, "data": 0}})
        assert blocked_button["error"], "Button hit injected into excluded owned PID"
        assert self.probes()[3]["native_event_count"] == before[3]["native_event_count"]
        assert not first.request("execute", request={"MoveTo": {"user": 0, "x": self.points[0][0], "y": self.points[0][1]}})["error"]
        for request in (self.key_request(1, 116, True), "Pause", {"Mode": {"mode": "concurrent", "multi_keyboard": True}}):
            reply = first.request("execute", request=request)
            assert reply["error"], "Unpermitted request was accepted"
            rejected.append({"client": reply["client"], "sequence": reply["sequence"], "error": reply["error"]})
            self.original_holds()
            assert self.probes()[0]["cursor"] == before[0]["cursor"]
            assert self.probes()[3]["native_event_count"] == before[3]["native_event_count"], "Excluded owned PID received native input"
            assert reply["policy"] == tagged[0]["policy"], "Rejected request changed policy"
        assert first.request("capture", user=0)["error"]
        mcp = self.mcp()
        tools = mcp.rpc("tools/list", {})["tools"]
        assert {t["name"] for t in tools} == self.expected_tools
        session = mcp.tool("agent_session")
        assert session["mode"] == "gui_connection" and session["connected"]
        mcp.tool("routing_start", {"users": ["Forbidden standalone"]}, error=True)
        mcp.tool("routing_capture", {"user": 0}, error=True)
        mcp.tool("routing_settings", {"user": 0, "settings": {"size": 30}}, error=True)
        mcp.tool("routing_replication", {"edges": []}, error=True)
        mcp.tool("routing_input", {"action": "key", "user": 2, "key": "f5", "pressed": True}, error=True)
        first.request("execute", request=self.key_request(0, 119, True))
        mcp.tool("routing_input", {"action": "key", "user": 0, "key": "f7", "pressed": True})
        mcp.tool("routing_input", {"action": "text", "user": 0, "text": "Agent café 🙂"})
        eventually(self.probes, lambda rs: rs[0]["text"] == "Agent café 🙂" and 118 in rs[0]["held_keys"] and 119 in rs[0]["held_keys"])
        mcp.close()
        eventually(self.probes, lambda rs: 118 not in rs[0]["held_keys"] and 119 in rs[0]["held_keys"] and 116 in rs[0]["held_keys"])
        assert self.owned[-1].poll() is None, "MCP EOF stopped the GUI"
        first.close()
        eventually(self.probes, lambda rs: rs[0]["held_keys"] == [116] and rs[1]["held_keys"] == [117])
        self.evidence["auth_ownership_scope"] = {"descriptor_acl": acl, "pipe_acl": first.security, "distinct_clients": [v["client"] for v in tagged], "same_sequence": [v["sequence"] for v in tagged], "rejected": rejected, "excluded_pid_button_error": blocked_button["error"], "allowed_focus_motion_over_excluded_hover": True, "mcp_session": session, "native_before": before, "native_after": self.probes(), "gui_alive_after_mcp_eof": True}

    def actual_excluded_focus_phase(self):
        client = self.raw()
        before = self.probes()
        assert 117 in before[1]["held_keys"]
        rejected = []
        dx = (self.points[0][0] - self.points[3][0]) // 8
        for request in (self.key_request(0, 120, True), {"MoveTo": {"user": 0, "x": self.points[0][0], "y": self.points[0][1]}}, {"Input": {"device": 0xf1000002, "kind": 0, "flags": 0, "x": dx, "y": 0, "data": 0}}):
            reply = client.request("execute", request=request)
            assert reply["error"], "Allowed candidate position hid an excluded actual native focus"
            rejected.append({"client": reply["client"], "sequence": reply["sequence"], "error": reply["error"]})
            assert self.probes()[3]["native_event_count"] == before[3]["native_event_count"], "Agent leaked native input into actual excluded focus"
            assert 117 in self.probes()[1]["held_keys"]
        client.close()
        self.evidence["actual_excluded_focus"] = {"rejected": rejected, "native_before": before, "native_after": self.probes(), "candidate_was_allowed_a": True, "actual_native_focus_was_operator_selected_d": True}

    def decode_capture(self, payload, metadata, name):
        path = self.run / f"{name}.png"
        path.write_bytes(payload)
        with Image.open(io.BytesIO(payload)) as image:
            image.load()
            assert image.mode == "RGBA" and list(image.size) == [metadata["width"], metadata["height"]]
            assert image.getpixel((10, 10)) == (255, 255, 255, 255)
            expected = tuple(self.probes()[0]["capture_color"]) + (255,)
            assert image.getpixel((50, 60)) == expected, "Selected-source actual pixels differ"
        assert metadata["pid"] == self.pids[0] and metadata["hwnd"] == self.probes()[0]["capture_hwnd"] and metadata["user_id"] == self.ids[0]
        return {"path": str(path), "metadata": metadata, "source_color": list(expected), "sha256": hashlib.sha256(payload).hexdigest()}

    def capture_phase(self):
        self.find_descriptor()
        self.current_mcp = self.mcp()
        response = self.current_mcp.rpc("tools/call", {"name": "routing_capture", "arguments": {"user": 0}})
        assert not response.get("isError")
        image = next(v for v in response["content"] if v["type"] == "image")
        metadata = json.loads(next(v["text"] for v in response["content"] if v["type"] == "text"))
        small = self.decode_capture(base64.b64decode(image["data"]), metadata, "connected-selected-source")
        self.evidence["capture_small_prefix"] = small
        self.save()
        capture, input_client = self.raw(), self.raw()
        hwnd = self.probes()[0]["capture_hwnd"]
        rect = Rect()
        assert user32.GetClientRect(hwnd, ctypes.byref(rect))
        original_size = [rect.right, rect.bottom]
        assert user32.SetWindowPos(hwnd, None, 0, 0, 4000, 3990, 0x16)
        actual_rect = Rect()
        assert user32.GetClientRect(hwnd, ctypes.byref(actual_rect))
        actual_pixels = actual_rect.right * actual_rect.bottom
        assert 15_000_000 <= actual_pixels <= 16_000_000
        # Child clips to its unchanged small owned root. No desktop/private pixel
        # capture and no covering the unrelated owned seat are introduced.
        done, result = threading.Event(), {}
        def worker():
            try:
                result["reply"] = capture.request("capture", user=0)
            except Exception as error:
                result["failure"] = repr(error)
            finally:
                done.set()
        started = time.monotonic()
        thread = threading.Thread(target=worker)
        thread.start()
        try:
            # The second actor's independent ACK guarantees routing work can
            # proceed while the first actor awaits real native/PNG work.
            assert not done.is_set(), "Capture did not overlap responsiveness observation"
            before = self.probes()[1]["key_event_count"]
            input_started = time.monotonic()
            reply = input_client.request("execute", request=self.key_request(1, 120, True))
            assert not reply["error"]
            eventually(self.probes, lambda rs: rs[1]["key_event_count"] == before + 1 and 120 in rs[1]["held_keys"])
            latency = time.monotonic() - input_started
            assert not done.is_set(), "Capture completed before actual native input proof"
            assert latency < 1.0, ("Native input blocked on capture", latency)
            assert not input_client.request("execute", request=self.key_request(1, 120, False))["error"]
            self.evidence["capture_responsiveness_prefix"] = {"pixels": actual_pixels, "requested_outer_dimensions": [4000, 3990], "actual_client_dimensions": [actual_rect.right, actual_rect.bottom], "native_other_seat_input_seconds": latency, "native_input_observed_before_capture_reply": True, "probes": self.probes()}
            self.save()
            thread.join(timeout=12)
            assert done.is_set() and "failure" not in result, result
            large_reply = result["reply"]
            if large_reply["error"]:
                assert "timed out" in large_reply["error"], large_reply["error"]
                large = {"declared_timeout": large_reply["error"], "pixels": actual_pixels}
            else:
                frame = large_reply["frame"]
                large_metadata = {k: v for k, v in frame.items() if k != "png"}
                large = self.decode_capture(bytes(frame["png"]), large_metadata, "connected-large-source")
            self.original_holds()
            self.evidence["capture"] = {"small_actual_png": small, "large_actual_job": large, "native_other_seat_input_seconds": latency, "native_input_observed_before_capture_reply": True, "capture_elapsed_seconds": time.monotonic() - started, "probes": self.probes()}
        finally:
            assert user32.SetWindowPos(hwnd, None, 0, 0, *original_size, 0x16)
            thread.join(timeout=12)
            capture.close()
            input_client.close()
        self.current_mcp.tool("routing_input", {"action": "key", "user": 0, "key": "f7", "pressed": True})
        eventually(self.probes, lambda rs: 118 in rs[0]["held_keys"])

    def close_clients(self):
        for connection in self.connections:
            connection.close()
        for client in self.clients:
            if client.process.poll() is None:
                try:
                    client.close()
                except Exception:
                    client.process.kill()
                    client.process.wait(timeout=10)

    def pipe_gone(self):
        handle = kernel32.CreateFileW(self.pipe_name, 0xC0000000, 0, None, 3, 0, None)
        if handle == INVALID_HANDLE:
            return ctypes.get_last_error() == 2
        kernel32.CloseHandle(handle)
        return False

    def positive_capabilities(self):
        settings = {"size": 38, "shape": "ring", "badge": "agent", "speed_x": 1}
        configured = self.final_raw.request("execute", request={"Settings": {"index": 0, "settings": settings}})
        assert not configured["error"]
        assert all(configured["profile"]["seats"][0]["cursor"][k] == v for k, v in settings.items())
        gui_cursor = eventually(lambda: _ui.read_json(self.run / "ui.state.json"), lambda states: states and states[0]["overlay"]["shape"] == "ring" and states[0]["overlay"]["badge"] == "agent")
        eventually(self.probes, lambda rs: not rs[0]["held_keys"] and 117 in rs[1]["held_keys"])
        mode = self.final_raw.request("execute", request={"Mode": {"mode": "switched", "multi_keyboard": False}})
        assert not mode["error"] and mode["profile"]["desktop"]["mode"] == "switched" and not mode["profile"]["desktop"]["multi_keyboard"]
        eventually(self.probes, lambda rs: all(not r["held_keys"] for r in rs))
        paused = self.final_raw.request("execute", request="Pause")
        assert not paused["error"] and "Paused" in paused["events"]
        self.evidence["positive_capabilities"] = {"configured_profile": configured["profile"], "actual_gui_cursor": gui_cursor[0], "mode_profile": mode["profile"]["desktop"], "pause_ack": {"client": paused["client"], "sequence": paused["sequence"], "events": [v for v in paused["events"] if isinstance(v, str)]}, "hold_release_was_explicitly_authorized_by_settings_mode_pause": True}

    def execute_gate(self):
        profile = {"version": 1, "concurrent": True, "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": self.pids},
            "seats": [{"id": self.ids[i], "name": f"Connected own {i + 1}", "mouse": f"virtual://mouse/{i + 1}", "keyboard": f"virtual://keyboard/{i + 1}", "desktop_start": self.points[i], "cursor": {"lock_window": False, "speed_x": 8 if i == 0 else 1}} for i in range(3)]}
        step = _ui.step
        steps = [step("input", "Input lab"), step("start", "Start routing")]
        for i in range(3):
            steps.extend([step(f"select-{i}", f"{i + 1} Hold click"), step(f"release-{i}", f"{i + 1} Release click")])
        steps.extend([step("manual-a", "Hold key", {"selected_user": 0, "key_name": "f5"}), step("manual-b", "Hold key", {"selected_user": 1, "key_name": "f6"}),
            step("agent-page", "Agent Access"), step("scope-user", "Agent user 1"), step("scope-pid", f"Agent target PID {self.pids[0]}"),
            step("control-draft-on", "Agent session control permission"), step("control-draft-off", "Agent session control permission"), step("enable", "Enable agent access"),
            step("focus-input", "Input lab"), step("move-d", "1 Move to point", point=self.points[3], user=0), step("focus-d", "1 Hold click"), step("release-d", "1 Release click"), step("scope-focus", "Agent Access"),
            step("restore-input", "Input lab"), step("move-a", "1 Move to point", point=self.points[0], user=0), step("restore-a", "1 Hold click"), step("restore-release-a", "1 Release click"), step("restore-manual-a", "Hold key", {"selected_user": 0, "key_name": "f5"}), step("capture-page", "Agent Access"),
            step("capture-checkbox", "Agent capture permission"), step("capture-policy", "Apply access policy", {"agent_users": ",".join(self.ids[:2]), "agent_pids": ",".join(map(str, self.pids[:2]))}),
            step("disable", "Disable agent access"), step("grant-session-control", "Agent session control permission"),
            step("re-enable", "Enable agent access", {"agent_users": ",".join(self.ids), "agent_pids": ",".join(map(str, self.pids[:3]))}),
            step("operator-resume", "Resume"), step("gui-pause-after-agent-resume", "Pause"), step("gui-resume-again", "Resume"), step("stop-session", "Stop routing"), step("stopped-agent-page", "Agent Access")])
        if self.args.scope_tail:
            steps = [v for v in steps if v["id"] not in {"select-2", "release-2", "scope-user", "scope-pid", "control-draft-on", "control-draft-off"}]
            next(v for v in steps if v["id"] == "enable")["fields"] = {"agent_users": self.ids[0], "agent_pids": str(self.pids[0]), "agent_capture": "false", "agent_session_control": "false"}
        screenshot_clock = None
        def verify(name, sample):
            nonlocal screenshot_clock
            probes, state = sample["probes"], sample["state"]
            if name.startswith("select-") and not probes[int(name[-1])]["buttons"] & 1:
                return False
            if name == "manual-a" and 116 not in probes[0]["held_keys"]:
                return False
            if name == "manual-b" and 117 not in probes[1]["held_keys"]:
                return False
            if name == "focus-d" and not probes[3]["buttons"] & 1:
                return False
            if name == "scope-focus" and name not in self.once:
                self.actual_excluded_focus_phase()
                self.once.add(name)
            if name == "restore-a" and not probes[0]["buttons"] & 1:
                return False
            if name == "restore-manual-a":
                if 116 not in probes[0]["held_keys"]:
                    return False
                self.original_holds()
            if name == "operator-resume" and name not in self.once:
                paused = self.final_raw.request("execute", request="Pause")
                resumed = self.final_raw.request("execute", request="Resume")
                assert not paused["error"] and "Paused" in paused["events"]
                assert not resumed["error"] and "Resumed" in resumed["events"]
                self.evidence["positive_capabilities"]["agent_resume_ack"] = {"client": resumed["client"], "sequence": resumed["sequence"], "events": [v for v in resumed["events"] if isinstance(v, str)]}
                self.once.add(name)
            if name == "gui-resume-again" and name not in self.once:
                assert not self.final_raw.request("execute", request=self.key_request(0, 118, True))["error"]
                eventually(self.probes, lambda rs: 118 in rs[0]["held_keys"])
                self.once.add(name)
            if name in ("enable", "capture-policy", "disable", "re-enable", "stopped-agent-page", "stop-session"):
                agent = _ui.read_json(self.run / "ui.agent.json")
                sample["agent"] = agent
                path = self.run / "ui.agent.acks.jsonl"
                sample["agent_acks"] = [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []
                if agent["pending"]:
                    return False
                if name in ("enable", "capture-policy", "re-enable"):
                    if not agent["policy_enabled"]:
                        return False
                    assert not agent["error"] and sample["agent_acks"][-1]["policy_enabled"]
                if name == "enable" and name not in self.once:
                    assert agent["selected_users"] == [self.ids[0]] and agent["allowed_pids"] == [self.pids[0]] and not agent["capture"] and not agent["session_control"]
                    if self.args.scope_tail:
                        self.find_descriptor()
                    else:
                        self.first_connection_phase()
                    self.once.add(name)
                if name == "capture-policy":
                    assert set(agent["selected_users"]) == set(self.ids[:2]) and set(agent["allowed_pids"]) == set(self.pids[:2]) and agent["capture"] and not agent["session_control"]
                    if name not in self.once:
                        self.capture_phase()
                        self.once.add(name)
                        screenshot_clock = time.time()
                    png = self.run / "ui.png"
                    if not png.exists() or png.stat().st_mtime < screenshot_clock:
                        return False
                    with Image.open(png) as image:
                        image.load()
                        image.save(self.run / "agent-access.png")
                    sample["owned_agent_access_screenshot"] = str(self.run / "agent-access.png")
                if name == "disable":
                    if agent["policy_enabled"]:
                        return False
                    if name not in self.once:
                        self.current_mcp.tool("routing_input", {"action": "key", "user": 0, "key": "f7", "pressed": True}, error=True)
                        self.current_mcp.close()
                        self.once.add(name)
                    eventually(self.probes, lambda rs: rs[0]["held_keys"] == [116] and rs[1]["held_keys"] == [117])
                    self.original_holds()
                    if self.descriptor.exists() or not self.pipe_gone():
                        return False
                    self.evidence["disarm"] = {"policy": agent, "probes": self.probes(), "descriptor_removed": True, "old_pipe_removed": True}
                if name == "re-enable" and name not in self.once:
                    old_descriptor = self.descriptor
                    self.find_descriptor()
                    assert self.descriptor != old_descriptor, "Disarmed descriptor was reused"
                    self.final_raw = self.raw()
                    assert agent["session_control"] and set(agent["selected_users"]) == set(self.ids)
                    self.positive_capabilities()
                    self.once.add(name)
                if name == "stop-session":
                    if any(r["held_keys"] or r["buttons"] for r in probes) or any(user32.IsWindow(u["overlay"]["hwnd"]) for u in state):
                        return False
                    if self.descriptor.exists() or not self.pipe_gone():
                        return False
                    assert not agent["policy_enabled"] and not agent["clients"]
                if name == "stopped-agent-page":
                    assert not agent["policy_enabled"] and not agent["clients"] and not agent["pending"]
            return True
        try:
            self.execute(profile, steps, verify)
        finally:
            self.close_clients()
            self.evidence["all_connected_mcp_processes_closed"] = all(c.process.poll() is not None for c in self.clients)
            self.evidence["all_raw_pipe_handles_closed"] = all(c.handle is None for c in self.connections)
            self.save()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--matrix", action="store_true", help="Print preparation criteria only, no native launch")
    parser.add_argument("--ready", action="store_true")
    parser.add_argument("--scope-tail", action="store_true", help="Remaining recipient/capture/control tail after accepted auth/EOF prefix")
    parser.add_argument("--executable", type=Path)
    parser.add_argument("--x86-executable", type=Path)
    parser.add_argument("--runtime", type=Path)
    args = parser.parse_args()
    if args.matrix:
        print(json.dumps(MATRIX, indent=2))
    else:
        assert args.ready and args.executable and args.x86_executable and args.runtime, "No live gate before producer READY and explicit image paths"
        gate = ConnectedGate(args)
        try:
            gate.fixtures()
            gate.execute_gate()
        finally:
            gate.close_clients()
            for owned in reversed(gate.owned):
                if owned.poll() is None:
                    owned.terminate()
                owned.wait(timeout=15)
            gate.evidence["owned_processes_closed"] = all(p.poll() is not None for p in gate.owned)
            if gate.descriptor is not None and gate.descriptor.exists():
                gate.evidence["descriptor_retained_after_runner_forced_termination"] = True
                gate.descriptor.unlink()
                gate.evidence["owned_stale_descriptor_removed_by_runner"] = True
            gate.save()


if __name__ == "__main__":
    main()
