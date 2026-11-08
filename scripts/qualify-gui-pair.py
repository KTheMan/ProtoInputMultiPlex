"""Disposable Windows hosted runner ONLY. Initial actual GUI/native/Rust-WASM pair.

All raw evidence stays private. Only public-result.json is uploadable. No retries,
relaunch, process termination, physical input, routing commands or authority writes.
The enclosing workflow provides an independent 240-second process deadline.
"""
import argparse
import base64
import ctypes
from ctypes import wintypes
import hashlib
import http.server
import json
import locale
import sys
import os
from pathlib import Path
import queue
import re
import shutil
import socket
import struct
import subprocess
import threading
import time
import urllib.request
from urllib.parse import urlsplit


class Rejected(Exception):
    pass


def require(ok):
    if not ok:
        raise Rejected()


def read(path):
    require(path.stat().st_size <= 4 * 1024 * 1024)
    return json.loads(path.read_text(encoding="utf-8-sig"))


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class CDP:
    """Bounded private RFC6455 transport; never emits DevTools or page payloads."""
    def __init__(self, address):
        url = urlsplit(address)
        require(url.scheme == "ws" and url.hostname == "127.0.0.1")
        self.sock = socket.create_connection((url.hostname, url.port), timeout=2)
        self.sock.settimeout(2)
        key = base64.b64encode(os.urandom(16)).decode()
        request = (f"GET {url.path} HTTP/1.1\r\nHost: 127.0.0.1:{url.port}\r\n"
                   f"Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
                   "Sec-WebSocket-Version: 13\r\n\r\n")
        self.sock.sendall(request.encode())
        self.buffer = b""
        while b"\r\n\r\n" not in self.buffer:
            self.buffer += self.sock.recv(4096)
            require(len(self.buffer) < 65536)
        headers, self.buffer = self.buffer.split(b"\r\n\r\n", 1)
        accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
        require(headers.startswith(b"HTTP/1.1 101 ") and accept in headers)
        self.ident = 0

    def exact(self, size):
        while len(self.buffer) < size:
            chunk = self.sock.recv(min(65536, size - len(self.buffer)))
            require(bool(chunk))
            self.buffer += chunk
        result, self.buffer = self.buffer[:size], self.buffer[size:]
        return result

    def send(self, payload, opcode=1):
        require(len(payload) <= 4 * 1024 * 1024)
        size = len(payload)
        header = bytes([0x80 | opcode])
        header += bytes([0x80 | size]) if size < 126 else (bytes([0x80 | 126]) + struct.pack("!H", size) if size < 65536 else bytes([0x80 | 127]) + struct.pack("!Q", size))
        mask = os.urandom(4)
        self.sock.sendall(header + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))

    def receive(self):
        first, second = self.exact(2)
        require(first & 0x80 and not second & 0x80)
        size = second & 127
        if size == 126:
            size = struct.unpack("!H", self.exact(2))[0]
        elif size == 127:
            size = struct.unpack("!Q", self.exact(8))[0]
        require(size <= 4 * 1024 * 1024)
        payload = self.exact(size)
        if first & 15 == 9:
            self.send(payload, 10)
            return None
        require(first & 15 == 1)
        return json.loads(payload)

    def call(self, method, params=None, session=None):
        self.ident += 1
        message = {"id": self.ident, "method": method, "params": params or {}}
        if session:
            message["sessionId"] = session
        self.send(json.dumps(message).encode())
        end = time.monotonic() + 5
        while time.monotonic() < end:
            value = self.receive()
            if value and value.get("id") == self.ident:
                require("error" not in value)
                return value["result"]
        raise Rejected()

    def evaluate(self, expression, session):
        result = self.call("Runtime.evaluate", {"expression": expression, "returnByValue": True,
                           "awaitPromise": True, "timeout": 3000}, session)
        require("exceptionDetails" not in result)
        return result["result"].get("value")


class StaticServer(http.server.ThreadingHTTPServer):
    allow_reuse_address = False

    def server_bind(self):
        self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
        super().server_bind()


class Pair:
    def __init__(self, args):
        self.args = args
        self.start = time.monotonic()
        self.end = self.start + 180
        self.stage = "preflight"
        self.checkpoint = 0
        self.check = "none"
        self.result = {"schema": "pim.gui-pair.initial.v1", "status": "FAIL",
                       "source_commit": args.commit, "scope": "initial actual GUI/native/Rust-WASM pair",
                       "full_sdk_complete": False, "P1_qualified": False, "release_qualified": False,
                       "hardware_qualified": False}
        self.gui = None

    def deadline(self):
        require(time.monotonic() < self.end)
        if self.gui:
            require(self.gui.poll() is None)

    def wait(self, fn, seconds=15):
        end = min(self.end, time.monotonic() + seconds)
        while time.monotonic() < end:
            self.deadline()
            try:
                value = fn()
            except (FileNotFoundError, json.JSONDecodeError):
                value = None
            if value:
                return value
            time.sleep(.05)
        raise Rejected()

    def launch(self, argv, name, env=None):
        # All children and their diagnostics are owned/private. No shell or secret argv.
        out = open(self.private / (name + "-stdout.log"), "xb")
        err = open(self.private / (name + "-stderr.log"), "xb")
        child = subprocess.Popen([str(x) for x in argv], cwd=self.private, env=env,
                                 stdout=out, stderr=err, creationflags=subprocess.CREATE_NO_WINDOW)
        out.close()
        err.close()
        return child

    def checked(self, root, entry, name):
        require(entry["file"] == name and re.fullmatch("[0-9a-f]{64}", entry["sha256"]))
        path = root / name
        require(digest(path) == entry["sha256"])
        return path

    def launch_status(self):
        path = self.ui / "prepare.launch.json"
        require(time.time() - path.stat().st_mtime < 2)
        progress_path = self.ui / "prepare.progress.json"
        require(time.time() - progress_path.stat().st_mtime < 2)
        progress = read(progress_path)
        require(progress["io"]["fault"] is None)
        value = read(path)
        require(all(value[k] is False for k in ("controller_present", "preflight_pending", "stopping")))
        return value

    def event_status(self, phase=None, users=None, approved=None, connections=None):
        value = self.launch_status()["setup"]["application_events"]
        require(value["sample_age_ms"] <= 2000)
        if phase is not None and value["phase"] != phase:
            return None
        if users is not None and value["local_virtual_users"] != users:
            return None
        if approved is not None and value["approved_receivers"] != approved:
            return None
        if connections is not None:
            if not (value.get("transport_sample_age_ms", 99999) <= 2000 and value.get("connected_clients") == connections and value.get("cleanup_pending") == 0):
                return None
            require(value.get("uncertain_admissions") == 0 and value.get("queue_overflows") == 0)
        return value

    def receipts(self):
        path = self.ui / "prepare.events.jsonl"
        if not path.exists():
            return []
        require(path.stat().st_size <= 1024 * 1024)
        return [json.loads(line) for line in path.read_text(encoding="utf-8-sig").splitlines()]

    def acknowledge(self, step):
        temp = self.ui / "prepare.sampled.tmp"
        temp.write_text(str(step), encoding="ascii")
        temp.replace(self.ui / "prepare.sampled.json")

    def descriptors(self):
        return set(self.preferences.parent.glob(f"protoinputmultiplex.application-events.{self.gui.pid}.*.json"))

    def issue(self, count):
        paths = self.descriptors()
        if len(paths) != count:
            return None
        new = paths - self.issued
        require(len(new) == 1)
        path = new.pop()
        data = read(path)
        require(set(data) == {"protoinputmultiplex_application_events", "endpoint", "exact_origin", "credential", "expires_unix_ms"})
        require(data["protoinputmultiplex_application_events"] == 1 and data["exact_origin"] == "http://127.0.0.1:4173")
        url = urlsplit(data["endpoint"])
        require(url.scheme == "ws" and url.hostname == "127.0.0.1" and url.port and url.path == "/application-events/v1" and not (url.query or url.fragment or url.username or url.password))
        require(re.fullmatch("[0-9a-fA-F]{64}", data["credential"]))
        require(250000 < data["expires_unix_ms"] - time.time() * 1000 <= 301000)
        match = re.fullmatch(rf"protoinputmultiplex\.application-events\.{self.gui.pid}\.(\d+)\.(\d+)\.json", path.name)
        require(match is not None)
        epoch = int(match[1])
        if count == 1:
            self.epoch = epoch
            self.native_descriptor, self.native_descriptor_hash = path, digest(path)
        else:
            require(epoch == self.epoch and digest(self.native_descriptor) == self.native_descriptor_hash)
            first = read(self.native_descriptor)
            require(first["endpoint"] == data["endpoint"] and first["credential"] != data["credential"])
            self.web_descriptor = path
        self.issued = paths
        return True

    def sample(self, step):
        if step == 5:
            path = self.ui / "prepare.discovery.json"
            require(time.time() - path.stat().st_mtime < 2)
            d = read(path)
            return d["completed_generation"] > 0 and not d["worker_failed"] and not d["errors"] and d["monitor_count"] > 0 and d["sample_age_ms"] <= 2000 and d.get("age_ms", 99999) <= 10000
        if step == 6:
            s = self.event_status("local", 0, 0)
            return s and not s["pointer_export"] and not s["keyboard_export"] and not self.descriptors()
        if step == 8:
            return self.event_status("local", 2, 0)
        if step in (15, 25, 35):
            s = self.event_status("local", 2, {15: 0, 25: 1, 35: 2}[step])
            return s and s["native_observed_users"] == 2 and s["pointer_export"] and s["keyboard_export"] and not self.descriptors()
        if step == 36:
            return self.event_status("active", 2, 2, 0) and not self.descriptors()
        if step in (38, 40):
            return self.issue(1 if step == 38 else 2)
        if step == 42:
            if not (self.launch_status()["setup"]["enabled"] and self.event_status("active", 2, 2, 0)):
                return None
            paths = list(self.temp.glob(f"protoinputmultiplex-agent-{self.gui.pid}-*.json"))
            if len(paths) != 1:
                return None
            data = read(paths[0])
            require(data["controller_pid"] == self.gui.pid and data["kind"] == "stopped_setup")
            self.setup_descriptor = paths[0]
            self.mcp_status(0)
            return True
        raise Rejected()

    def mcp_status(self, connections):
        self.stage = "read-only-stopped-setup"
        # One authenticated MCP subprocess; the controller sends only initialize and status.
        if not hasattr(self, "mcp"):
            err = open(self.private / "mcp-stderr.log", "xb")
            self.mcp = subprocess.Popen([str(self.exe), "mcp", "--gui-connection", str(self.setup_descriptor)],
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err,
                                        text=True, encoding="utf-8", creationflags=subprocess.CREATE_NO_WINDOW)
            err.close()
            self.mcp_queue = queue.Queue()
            def reader():
                try:
                    for line in self.mcp.stdout:
                        require(len(line) <= 4 * 1024 * 1024)
                        self.mcp_queue.put(json.loads(line))
                finally:
                    self.mcp_queue.put(None)
            threading.Thread(target=reader, daemon=True).start()
            self.rpc_id = 0
            self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "pim-initial-pair", "version": "1"}})
            self.mcp.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
            self.mcp.stdin.flush()
        reply = self.rpc("tools/call", {"name": "application_event_status", "arguments": {}})
        require(not reply.get("isError"))
        value = reply.get("structuredContent") or json.loads(reply["content"][0]["text"])
        # Stopped Setup's standalone status tool returns PublicStatus directly.
        # Only Script launch snapshots nest it under setup.application_events.
        s = value
        require(s["phase"] == "active" and s["session_active"] and s["local_virtual_users"] == s["native_observed_users"] == 2 and s["approved_receivers"] == 2 and s["pointer_export"] and s["keyboard_export"])
        require(s["sample_age_ms"] <= 2000 and s.get("transport_sample_age_ms", 99999) <= 2000 and s.get("connected_clients") == connections and s.get("cleanup_pending") == 0)
        self.launch_status()

    def rpc(self, method, params):
        self.rpc_id += 1
        self.mcp.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.rpc_id, "method": method, "params": params}) + "\n")
        self.mcp.stdin.flush()
        end = min(self.end, time.monotonic() + 8)
        while time.monotonic() < end:
            self.deadline()
            try:
                result = self.mcp_queue.get(timeout=.1)
            except queue.Empty:
                continue
            require(result is not None and "error" not in result)
            if result.get("id") == self.rpc_id:
                return result["result"]
        raise Rejected()

    def web(self, expression):
        return self.cdp.evaluate(expression, self.session)

    def state(self):
        # Keep the Rust-produced JSON as a STRING across CDP/JS. Python ints are exact.
        raw = self.web("window.pimClient ? window.pimClient.state_json() : null")
        if not raw:
            return None
        value = json.loads(raw)
        return value if value["stream_state"] == "Live" else None

    @staticmethod
    def identities(users):
        require(len(users) == 2)
        result = sorted((u["id"]["epoch"], u["id"]["local"], u["source"]) for u in users)
        require(result[0][:2] != result[1][:2] and result[0][2] != result[1][2])
        return result

    def grants(self, events, controls, ids):
        require(events["pointer_events"] and events["key_events"])
        for grant in (events, controls):
            require(set(grant["users"]) == {"kind", "users"} and grant["users"]["kind"] == "only")
            actual = sorted((x["epoch"], x["local"]) for x in grant["users"]["users"])
            require(actual == [x[:2] for x in ids])
        require(all(controls[k] for k in ("delegated_users", "user_lifecycle", "cursor", "pointer_input", "key_input")))
        require(not controls["window_tracking"] and not controls["window_control"])

    def native_rows(self):
        if not self.native_report.exists():
            return []
        require(self.native_report.stat().st_size <= 4 * 1024 * 1024)
        text = self.native_report.read_text(encoding="utf-8")
        return [json.loads(line) for line in text[:text.rfind("\n") + 1].splitlines()]

    def native_detail(self, name):
        rows = self.native_rows()
        require(not any(r["stage"] == "failed" for r in rows))
        matches = [r["detail"] for r in rows if r["stage"] == name]
        require(len(matches) <= 1)
        return matches[0] if matches else None

    def failure_observations(self):
        # Fixed scalars only. Never serialize a label, fault string, path or raw file.
        def boolean(value):
            return value if type(value) is bool else None
        def count(value, maximum):
            return value if type(value) is int and 0 <= value <= maximum else None
        def age(path):
            value = int((time.time() - path.stat().st_mtime) * 1000)
            return count(value, 3600000)
        codec = locale.getpreferredencoding(False).lower()
        if re.fullmatch(r"utf-?8|cp[0-9]{1,5}|ascii|ansi_x3\.4-1968", codec) is None:
            codec = "other"
        result = {"schema": "pim.gui-pair.failure-observations.v1",
                  "checkpoint": count(self.checkpoint, 42), "check": self.check,
                  "receipt_reader_encoding": "utf-8-sig",
                  "python_major": int(sys.version_info.major), "python_minor": int(sys.version_info.minor),
                  "python_utf8_mode": bool(sys.flags.utf8_mode), "runtime_default_text_encoding": codec,
                  "receipt_count": None, "receipt_count_out_of_bounds": None,
                  "receipt_prefix_matches": None, "progress_next_matches_expected": None,
                  "progress_label_matches_expected": None, "response_enabled": None,
                  "response_visible": None, "awaiting_click": None, "progress_age_ms": None,
                  "io_fault_present": None, "io_active": None, "io_submitted_sequence": None,
                  "io_acked_sequence": None, "io_sampled": None, "io_pending_snapshots": None,
                  "io_pending_critical": None, "launch_age_ms": None, "event_phase": None,
                  "event_sample_age_ms": None, "transport_sample_age_ms": None,
                  "local_virtual_users": None, "native_observed_users": None,
                  "approved_receivers": None, "connected_clients": None, "cleanup_pending": None,
                  "diagnostic_read_failed": False}
        try:
            rows = self.receipts()
            result["receipt_count"] = count(len(rows), 42)
            result["receipt_count_out_of_bounds"] = len(rows) > 42
            result["receipt_prefix_matches"] = len(rows) <= len(self.steps) and all(
                row.get("button") == self.steps[index]["button"] and row.get("clicked") is True
                for index, row in enumerate(rows))
        except BaseException:
            result["diagnostic_read_failed"] = True
        try:
            path = self.ui / "prepare.progress.json"
            progress = read(path)
            result["progress_age_ms"] = age(path)
            if 1 <= self.checkpoint <= 42 and len(self.steps) == 42:
                expected = self.steps[self.checkpoint - 1]["button"]
                result["progress_next_matches_expected"] = progress.get("next") == expected
                result["progress_label_matches_expected"] = progress.get("response_label") == expected
            for key in ("response_enabled", "response_visible", "awaiting_click"):
                result[key] = boolean(progress.get(key))
            io = progress["io"]
            result["io_fault_present"] = io.get("fault") is not None
            result["io_active"] = boolean(io.get("active"))
            for key in ("submitted_sequence", "acked_sequence"):
                result["io_" + key] = count(io.get(key), 10000000)
            result["io_sampled"] = count(io.get("sampled"), 42)
            result["io_pending_snapshots"] = count(io.get("pending_snapshots"), 1000)
            result["io_pending_critical"] = count(io.get("pending_critical"), 1000)
        except BaseException:
            result["diagnostic_read_failed"] = True
        try:
            path = self.ui / "prepare.launch.json"
            launch = read(path)
            result["launch_age_ms"] = age(path)
            status = launch["setup"]["application_events"]
            phase = status.get("phase")
            result["event_phase"] = phase if phase in ("off", "local", "starting", "active", "stopping", "stopped", "retired", "failed") else "other"
            result["event_sample_age_ms"] = count(status.get("sample_age_ms"), 3600000)
            result["transport_sample_age_ms"] = count(status.get("transport_sample_age_ms"), 3600000)
            for key in ("local_virtual_users", "native_observed_users"):
                result[key] = count(status.get(key), 64)
            for key in ("approved_receivers", "connected_clients", "cleanup_pending"):
                result[key] = count(status.get(key), 8)
        except BaseException:
            result["diagnostic_read_failed"] = True
        return result

    def hook_messages(self):
        raw = self.web("JSON.stringify(window.pimEvents)")
        return [json.loads(value) for value in json.loads(raw)]

    def hook_data(self):
        # All identities/envelopes remain private JSON strings across JS/CDP.
        raw = self.web("JSON.stringify(window.pimHook.data())")
        return json.loads(raw)

    def hook_prepare(self, ids):
        self.stage = "web-hooks-register"
        self.hook_ids = [{"epoch": value[0], "local": value[1]} for value in ids]
        source = r"""(() => {
          const c=window.pimClient;
          const h=window.pimHook={a:[],b:[],ua:[],ub:[],screens:[],once:[],late:[],victim:[],
            clicks:[],ca:[],cb:[],lateClicks:[],throws:0,domClicks:0,removed:false,cleanup:0,afterDispose:0,t:{}};
          const save=k=>x=>{if(h[k].length>=512)throw Error('hook budget');h[k].push(x);};
          h.t.a=c.add_event_listener('{"kind":"input"}',save('a'));
          h.t.throw=c.add_event_listener('{"kind":"input"}',()=>{h.throws++;throw Error('consumer probe');});
          h.t.b=c.add_event_listener('{"kind":"input"}',save('b'));
          h.t.ua=c.add_event_listener(FILTER_A,save('ua'));
          h.t.ub=c.add_event_listener(FILTER_B,save('ub'));
          h.t.screens=c.add_event_listener('{"kind":"screens"}',save('screens'));
          h.t.once=c.add_event_listener('{"kind":"input"}',x=>{
            h.once.push(x);h.read=c.state_json();h.selfRemoved=c.remove_listener(h.t.once);c.invalidate_virtual_clicks();
          });
          h.t.remover=c.add_event_listener('{"kind":"input"}',()=>{
            h.removed=c.remove_listener(h.t.victim);h.t.late=c.add_event_listener('{"kind":"input"}',save('late'));
            c.remove_listener(h.t.remover);
          });
          h.t.victim=c.add_event_listener('{"kind":"input"}',save('victim'));
          const root=document.getElementById('application-demo'),canvas=root.querySelector('[data-canvas]');
          const r=canvas.getBoundingClientRect(),target=document.createElement('div');
          target.dataset.hookTarget='yes';target.textContent='Virtual callback acceptance target';
          target.style.cssText=`position:fixed;left:${r.left+r.width*.25}px;top:${r.top+r.height*.2}px;width:${r.width*.5}px;height:${r.height*.25}px;z-index:1000;background:#305070`;
          root.appendChild(target);h.target=target;h.originalLeft=target.style.left;h.focus=document.activeElement;
          target.addEventListener('click',()=>{h.domClicks++;});c.invalidate_virtual_clicks();
          h.t.click=c.bind_virtual_click('{"selector":"[data-hook-target]","button":"primary"}',save('clicks'));
          h.t.ca=c.bind_virtual_click(CLICK_A,save('ca'));
          h.t.cb=c.bind_virtual_click(CLICK_B,save('cb'));
          h.data=()=>({a:h.a,b:h.b,ua:h.ua,ub:h.ub,screens:h.screens,once:h.once,late:h.late,victim:h.victim,
            clicks:h.clicks,ca:h.ca,cb:h.cb,lateClicks:h.lateClicks,throws:h.throws,domClicks:h.domClicks,
            removed:h.removed,selfRemoved:h.selfRemoved,cleanup:h.cleanup,afterDispose:h.afterDispose,
            focusUnchanged:document.activeElement===h.focus});
          return Object.values(h.t).every(x=>typeof x==='string' && x.length>0);
        })()"""
        for label, value in (("FILTER_A", {"kind": "input", "user": self.hook_ids[0]}),
                             ("FILTER_B", {"kind": "input", "user": self.hook_ids[1]}),
                             ("CLICK_A", {"selector": "[data-hook-target]", "button": "primary", "user": self.hook_ids[0]}),
                             ("CLICK_B", {"selector": "[data-hook-target]", "button": "primary", "user": self.hook_ids[1]})):
            source = source.replace(label, json.dumps(json.dumps(value, separators=(",", ":"))))
        require(self.web(source))
        # Registration alone preserves the existing disconnected-production gates.
        require(self.web("!document.querySelector('[data-pointer]').checked && !document.querySelector('[data-keys]').checked && document.querySelector('[data-users]').value===''") )

    def hook_select(self, user):
        encoded = json.dumps(json.dumps(user, separators=(",", ":")))
        require(self.web("window.pimClient.select_user(" + encoded + ");window.pimClient.enable_pointer(true);document.querySelector('[data-pointer]').checked && !document.querySelector('[data-keys]').checked"))

    def hook_native_input(self, sequence, user, value):
        rows = [row["detail"] for row in self.native_rows() if row["stage"] == "input_observed" and row["detail"]["sequence"] == sequence]
        require(len(rows) <= 1)
        if not rows:
            return None
        row = rows[0]
        require(row["epoch"] == self.epoch and row["user"] == user and row["input"] == value)
        require((user["epoch"], user["local"], row["source"]) in self.hook_exact_ids)
        return row

    def hook_input(self, user, value):
        require(self.native.poll() is None)
        command = {"kind": "input_at_revision", "user": user, "input": value,
                   "transform_revision": self.hook_revision}
        encoded = json.dumps(json.dumps(command, separators=(",", ":")))
        expected = self.hook_request_id + 1
        require(self.web("window.pimClient.request(" + encoded + ");true"))
        def observed():
            messages = self.hook_messages()
            replies = [x for x in messages if x.get("kind") == "response" and x["request_id"] > self.hook_request_id]
            if not replies:
                return None
            require(len(replies) == 1 and replies[0]["request_id"] == expected)
            reply = replies[0]["reply"]
            require(reply["status"] == "ok" and reply["version"] == 1 and reply["response"]["kind"] == "input_accepted")
            sequences = reply["response"]["sequences"]
            require(len(sequences) == 1)
            sequence = sequences[0]
            events = [e for x in messages if x.get("kind") == "event_stream" and x["stream"]["kind"] == "events" for e in x["stream"]["events"] if e["sequence"] == sequence]
            if not events:
                return None
            require(len(events) == 1)
            envelope = events[0]
            event = envelope["event"]
            require(envelope["version"] == 1 and envelope["epoch"] == self.epoch and event["kind"] == "input"
                    and event["user"] == user and event["input"] == value and event["transform_revision"] == self.hook_revision)
            native = self.hook_native_input(sequence, user, value)
            if not native:
                return None
            require(all(native[key] == event[key] for key in ("user", "source", "input", "position", "transform_revision")))
            return envelope
        envelope = self.wait(observed, 5)
        self.hook_request_id = expected
        self.hook_acknowledgments += 1
        return envelope

    @staticmethod
    def hook_frame_has(raw, user):
        message = json.loads(raw)
        return message.get("kind") == "event_stream" and message["stream"]["kind"] == "events" and any(
            e["event"]["kind"] == "input" and e["event"]["user"] == user for e in message["stream"]["events"])

    def hook_counts(self, any_count, a_count, b_count, late_count=0):
        data = self.hook_data()
        require([len(data[key]) for key in ("clicks", "ca", "cb", "lateClicks")] == [any_count, a_count, b_count, late_count])
        require(data["domClicks"] == 0 and data["focusUnchanged"])
        return data

    def hook_cases(self, ids):
        self.stage = "web-hooks-actual-virtual-input"
        self.hook_exact_ids = ids
        self.hook_acknowledgments = 0
        state = self.state()
        require(state is not None)
        self.hook_revision = state["screens"]["revision"]
        self.hook_request_id = max(x["request_id"] for x in self.hook_messages() if x.get("kind") == "response")
        # Only geometry is converted to floating point. Identities stay exact text.
        rect = self.web("JSON.stringify((()=>{const c=document.querySelector('[data-canvas]').getBoundingClientRect(),t=window.pimHook.target.getBoundingClientRect();return {canvas:[c.left,c.top,c.width,c.height],point:[t.left+t.width/2,t.top+t.height/2],visible:document.elementFromPoint(t.left+t.width/2,t.top+t.height/2)===window.pimHook.target};})())")
        geometry = json.loads(rect)
        require(geometry["visible"])
        canvas, point = geometry["canvas"], geometry["point"]
        bounds = state["screens"]["desktop"]
        position = {"x": round(bounds["left"] + (point[0] - canvas[0]) / canvas[2] * (bounds["right"] - bounds["left"])),
                    "y": round(bounds["top"] + (point[1] - canvas[1]) / canvas[3] * (bounds["bottom"] - bounds["top"]))}
        down = {"kind": "button", "button": "primary", "transition": "down"}
        up = {"kind": "button", "button": "primary", "transition": "up"}
        releases = []
        for index, user in enumerate(self.hook_ids):
            self.hook_select(user)
            self.hook_input(user, {"kind": "motion", "position": position})
            self.hook_input(user, down)
            releases.append(self.hook_input(user, up))
            data = self.hook_counts(index + 1, 1, index)
            click = json.loads(data["clicks"][-1])
            require(click["kind"] == "virtual_click" and click["source"] == "authorized_virtual_stream"
                    and click["selector"] == "[data-hook-target]" and click["event"] == releases[-1])
        data = self.hook_counts(2, 1, 1)
        require(json.loads(data["ca"][0])["event"] == releases[0] and json.loads(data["cb"][0])["event"] == releases[1])
        user = self.hook_ids[0]
        self.hook_select(user)
        self.hook_input(user, {"kind": "motion", "position": position})
        self.hook_input(user, down)
        require(self.web("window.pimClient.invalidate_virtual_clicks();true"))
        self.hook_input(user, up)
        self.hook_counts(2, 1, 1)
        self.hook_input(user, down)
        require(self.web("window.pimHook.target.style.left=(parseFloat(window.pimHook.originalLeft)+3)+'px';true"))
        self.hook_input(user, up)
        self.hook_counts(2, 1, 1)
        require(self.web("window.pimHook.target.style.left=window.pimHook.originalLeft;window.pimClient.invalidate_virtual_clicks();true"))
        # A fresh binding registered while the Host reports held Down has no press.
        require(self.web("['click','ca','cb'].every(k=>window.pimClient.remove_listener(window.pimHook.t[k]) && !window.pimClient.remove_listener(window.pimHook.t[k]))"))
        self.hook_input(user, down)
        require(self.web("window.pimHook.t.lateClick=window.pimClient.bind_virtual_click('{\"selector\":\"[data-hook-target]\",\"button\":\"primary\"}',x=>window.pimHook.lateClicks.push(x));typeof window.pimHook.t.lateClick==='string' && window.pimHook.t.lateClick!==window.pimHook.t.click"))
        self.hook_input(user, up)
        self.hook_counts(2, 1, 1)
        # Legitimate refresh releases this producer's hold and fences pending pair.
        self.hook_input(user, down)
        before = self.hook_request_id
        require(self.web("window.pimClient.refresh_subscription();true"))
        def refreshed():
            replies = [x for x in self.hook_messages() if x.get("kind") == "response" and x["request_id"] > before]
            if len(replies) < 2 or self.state() is None:
                return None
            require(len(replies) == 2 and [x["request_id"] for x in replies] == [before + 1, before + 2])
            first, second = [x["reply"] for x in replies]
            require(first["status"] == second["status"] == "ok" and first["response"]["kind"] == "input_accepted" and second["response"]["kind"] == "subscribed")
            sequences = first["response"]["sequences"]
            require(len(sequences) == 1)
            return self.hook_native_input(sequences[0], user, up)
        self.wait(refreshed, 5)
        self.hook_request_id = before + 2
        self.hook_counts(2, 1, 1)
        require(self.web("!document.querySelector('[data-pointer]').checked && !document.querySelector('[data-keys]').checked && document.querySelector('[data-users]').value===''") )
        self.hook_select(user)
        self.hook_input(user, down)
        final_release = self.hook_input(user, up)
        data = self.hook_counts(2, 1, 1, 1)
        require(json.loads(data["lateClicks"][0])["event"] == final_release)
        require(data["a"] == data["b"] and len(data["a"]) >= 2 and len(set(data["a"])) == len(data["a"]))
        require(data["throws"] == len(data["b"]) and len(data["once"]) == 1 and data["selfRemoved"] and data["removed"] and not data["victim"])
        require(data["late"] and data["once"][0] not in data["late"])
        require(data["ua"] and data["ub"] and all(self.hook_frame_has(raw, self.hook_ids[0]) for raw in data["ua"])
                and all(self.hook_frame_has(raw, self.hook_ids[1]) for raw in data["ub"]) and not data["screens"])
        a_count, b_count = len(data["a"]), len(data["b"])
        require(self.web("window.pimClient.remove_listener(window.pimHook.t.a) && !window.pimClient.remove_listener(window.pimHook.t.a)"))
        position["x"] += 1
        self.hook_input(user, {"kind": "motion", "position": position})
        data = self.hook_counts(2, 1, 1, 1)
        require(len(data["a"]) == a_count and len(data["b"]) == b_count + 1)
        require(self.state() is not None and self.identities(self.state()["users"]) == ids)
        require(all(u["held_keys"] == [] and u["pointer"]["held_buttons"] == [] for u in self.state()["users"]))
        require(self.hook_acknowledgments == 17)
        self.result["web_event_hooks"] = {"actual_wasm": True, "fanout_filters_remove_throw": True,
            "two_user_virtual_clicks": 2, "invalidation_geometry_unpaired_up": True,
            "refresh_pair_cleanup": True, "web_to_native_correlated_input_acks": self.hook_acknowledgments,
            "legacy_preserved": True, "gap_runtime_qualified": False}

    def hook_cleanup(self):
        self.stage = "web-hooks-reentrant-dispose"
        source = r"""(()=>{const c=window.pimClient,h=window.pimHook;
          h.t.cleanup=c.add_event_listener('{"kind":"local"}',()=>{
            h.cleanup++;h.cleanupState=c.state_json();c.remove_listener(h.t.cleanup);c.dispose();
          });
          h.t.after=c.add_event_listener('{"kind":"local"}',()=>{h.afterDispose++;});
          c.disconnect();c.dispose();
          let rejected=0;
          try{c.add_event_listener('{}',()=>{});}catch(_){rejected++;}
          try{c.bind_virtual_click('{"selector":"[data-hook-target]"}',()=>{});}catch(_){rejected++;}
          try{c.connect('','');}catch(_){rejected++;}
          return h.cleanup===1 && h.afterDispose===0 && rejected===3 && !c.remove_listener(h.t.b);
        })()"""
        require(self.web(source))
        data = self.hook_data()
        require(data["cleanup"] == 1 and data["afterDispose"] == 0 and data["domClicks"] == 0)
        self.result["web_event_hooks"]["reentrant_dispose_cleanup"] = True

    def run(self):
        a = self.args
        require(os.name == "nt" and os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted")
        require(re.fullmatch("[0-9a-f]{40}", a.commit) is not None)
        self.private = a.run.resolve()
        require(not self.private.exists())
        self.private.mkdir()
        self.ui, self.temp = self.private / "ui", self.private / "setup-temp"
        self.ui.mkdir()
        self.temp.mkdir()
        prefs = self.private / "preferences"
        prefs.mkdir()
        self.preferences = prefs / "app-preferences.json"
        shutil.copyfile(a.source / "fixtures/gui-sdk-preparation/app-preferences.json", self.preferences)
        require(read(self.preferences)["close_to_tray"] is False)
        self.steps = read(a.source / "fixtures/gui-sdk-preparation/prepare.json")
        require(len(self.steps) == 42)
        shutil.copyfile(a.source / "fixtures/gui-sdk-preparation/prepare.json", self.ui / "prepare.json")
        gui_root = a.artifacts / "sdk-preview/gui"
        native_root = a.artifacts / "sdk-preview/x64"
        gui_meta, native_meta = read(gui_root / "metadata.json"), read(native_root / "metadata.json")
        require(gui_meta["source_commit"] == native_meta["source_commit"] == a.commit)
        require(gui_meta["features"] == ["software-renderer"] and gui_meta["default_features"] is True and gui_meta["target"] == "x86_64-pc-windows-msvc")
        require(native_meta["features"] == ["native-ffi", "native-windows", "native-client"] and native_meta["default_features"] is True and native_meta["native_client_abi_version"] == 2)
        self.exe = self.checked(gui_root, gui_meta["executable"], "protoinputmultiplex.exe")
        native_exe = self.checked(native_root, native_meta["external_fixture"], "native_external_consumer.exe")
        dll = self.checked(native_root, native_meta["dll"], "mux_application_sdk.dll")
        web_root = a.artifacts / "application-web"
        web_meta = read(web_root / "metadata.json")
        require(web_meta["source_commit"] == a.commit and web_meta["rust_wasm"] is True)
        for entry in web_meta["files"]:
            require(entry["file"] in ("index.html", "bootstrap.js", "pkg/mux_application_web.js", "pkg/mux_application_web_bg.wasm"))
            self.checked(web_root, entry, entry["file"])
        require({x["file"] for x in web_meta["files"]} == {"index.html", "bootstrap.js", "pkg/mux_application_web.js", "pkg/mux_application_web_bg.wasm"})
        self.result["artifacts"] = {"gui": gui_meta["executable"]["sha256"], "dll": native_meta["dll"]["sha256"], "native_fixture": native_meta["external_fixture"]["sha256"], "web": web_meta["files"]}
        # Serve only explicit public WASM/demo files; never descriptors/runner roots.
        public = self.private / "static"
        shutil.copytree(web_root, public)
        (public / "pair.js").write_text("import {client} from './bootstrap.js'; window.pimClient=client; window.pimEvents=[]; client.set_event_handler(x=>{ if(window.pimEvents.length>=1024) throw Error('budget'); window.pimEvents.push(x); });", encoding="utf-8")
        (public / "pair.html").write_text('<!doctype html><div id="application-demo"></div><script type="module" src="./pair.js"></script>', encoding="utf-8")
        class Handler(http.server.SimpleHTTPRequestHandler):
            def __init__(self, *args, **kwargs):
                super().__init__(*args, directory=str(public), **kwargs)
            def log_message(self, *args):
                pass
        self.http = StaticServer(("127.0.0.1", 4173), Handler)
        threading.Thread(target=self.http.serve_forever, daemon=True).start()
        self.stage = "gui-preparation"
        env = dict(os.environ, TEMP=str(self.temp), TMP=str(self.temp))
        self.gui = self.launch([self.exe, "gui", "--renderer", "software-dx12", "--preferences", self.preferences, "--ui-script", self.ui / "prepare.json", "--ui-script-no-screenshots"], "gui", env)
        self.issued = set()
        for index, step in enumerate(self.steps, 1):
            self.checkpoint = index
            self.check = "receipt_count"
            self.wait(lambda: len(self.receipts()) >= index)
            rows = self.receipts()
            self.check = "receipt_count_or_order"
            require(len(rows) == index or not step.get("wait_for_sample"))
            self.check = "receipt_label"
            require(rows[index - 1]["button"] == step["button"] )
            self.check = "receipt_clicked"
            require(rows[index - 1]["clicked"] is True)
            if step.get("wait_for_sample"):
                self.check = "sample"
                self.wait(lambda: self.sample(index))
                self.check = "sample_ack"
                self.acknowledge(index)
            self.check = "none"
        # Start an isolated software browser once; DevTools is loopback and ephemeral.
        self.stage = "actual-rust-wasm-connect"
        edge = Path(os.environ["ProgramFiles(x86)"]) / "Microsoft/Edge/Application/msedge.exe"
        require(edge.is_file())
        profile = self.private / "edge-profile"
        self.browser = self.launch([edge, "--headless=new", "--window-size=1280,1000", "--disable-gpu", "--disable-extensions", "--no-first-run", "--no-default-browser-check", "--remote-debugging-address=127.0.0.1", "--remote-debugging-port=0", "--user-data-dir=" + str(profile), "about:blank"], "edge")
        def devtools():
            p = profile / "DevToolsActivePort"
            if not p.exists():
                return None
            lines = p.read_text(encoding="utf-8-sig").splitlines()
            return lines if len(lines) == 2 and lines[0].isdigit() else None
        lines = self.wait(devtools)
        self.cdp = CDP("ws://127.0.0.1:" + lines[0] + lines[1])
        target = self.cdp.call("Target.createTarget", {"url": "http://127.0.0.1:4173/pair.html"})["targetId"]
        self.session = self.cdp.call("Target.attachToTarget", {"targetId": target, "flatten": True})["sessionId"]
        self.wait(lambda: self.web("!!window.pimClient"))
        require(self.web("document.querySelector('[data-pointer]').checked===false && document.querySelector('[data-keys]').checked===false && document.querySelector('[data-pointer]').disabled && document.querySelector('[data-keys]').disabled"))
        descriptor = read(self.web_descriptor)
        # Credential travels only over the private local DevTools socket into Rust.
        # Never an HTTP endpoint, URL, argv, proxy, screenshot or evidence field.
        self.web("window.pimClient.connect(" + json.dumps(descriptor["endpoint"]) + "," + json.dumps(descriptor["credential"]) + "); true")
        del descriptor
        baseline = self.wait(self.state)
        ids = self.identities(baseline["users"])
        require(all(x[0] == self.epoch for x in ids))
        self.grants(baseline["event_grant"], baseline["control_grant"], ids)
        require(self.web("!document.querySelector('[data-pointer]').checked && !document.querySelector('[data-keys]').checked && document.querySelector('[data-users]').value===''"))
        self.wait(lambda: self.event_status("active", 2, 2, 1))
        self.hook_prepare(ids)
        self.stage = "native-pulse-overlap"
        self.native_report = self.private / "native.jsonl"
        self.native = self.launch([native_exe, "--dll", dll, "--sha256", native_meta["dll"]["sha256"], "--descriptor", self.native_descriptor, "--report", self.native_report, "--virtual-key", "4", "--observe-seconds", "60"], "native")
        snap = self.wait(lambda: self.native_detail("gui_snapshot"))
        require(self.identities(snap["users"]) == ids)
        require(self.native_detail("started")["observation_seconds"] == 60)
        ready = self.wait(lambda: self.native_detail("gui_ready"))
        require(ready["version"] == 1 and ready["epoch"] == self.epoch)
        self.grants(ready["events"], ready["controls"], ids)
        self.wait(lambda: self.event_status("active", 2, 2, 2))
        self.mcp_status(2)
        # Browser handler receives raw Rust-validated JSON as strings: no Number parse.
        def pulse():
            raw = self.web("JSON.stringify(window.pimEvents)")
            messages = [json.loads(x) for x in json.loads(raw)]
            events = []
            for message in messages:
                if message.get("kind") == "event_stream":
                    stream = message["stream"]
                    require(stream["kind"] == "events")
                    events.extend(stream["events"])
            receipts = [r["detail"] for r in self.native_rows() if r["stage"] == "virtual_key_acknowledged"]
            if len(receipts) != 2:
                return None
            require([r["transition"] for r in receipts] == ["down", "up"])
            for receipt in receipts:
                matches = [e for e in events if e["sequence"] in receipt["sequences"]]
                if not matches:
                    return None
                require(len(matches) == 1)
                event = matches[0]["event"]
                require(event["kind"] == "input" and event["user"] == receipt["user"] and event["input"] == {"kind": "key", "key": 4, "transition": receipt["transition"]})
                require(matches[0]["epoch"] == self.epoch and (event["user"]["epoch"], event["user"]["local"], event["source"]) in ids)
            return True
        self.wait(pulse)
        require(self.wait(lambda: self.native_detail("default_off_checked"))["locally_rejected"])
        self.hook_cases(ids)
        # Preserve repeated fresh same-Host observations throughout the native window.
        end = min(self.end, time.monotonic() + 70)
        samples = 0
        while self.native.poll() is None and time.monotonic() < end:
            self.deadline()
            value = self.state()
            require(value is not None and self.identities(value["users"]) == ids)
            require(self.event_status("active", 2, 2) is not None)
            samples += 1
            time.sleep(.2)
        require(self.native.poll() == 0 and samples >= 2)
        require(self.native_detail("dll_unloaded_before_owner_exit") is not None and self.native_detail("owner_ready_to_exit_after_unload") is not None)
        require(self.identities(self.native_detail("overlap_snapshot")["users"]) == ids)
        final_web = self.state()
        require(final_web is not None and self.identities(final_web["users"]) == ids)
        require(all(u["held_keys"] == [] and u["pointer"]["held_buttons"] == [] for u in final_web["users"]))
        self.wait(lambda: self.event_status("active", 2, 2, 1))
        self.mcp_status(1)
        self.stage = "normal-cleanup"
        self.hook_cleanup()
        self.wait(lambda: self.event_status("active", 2, 2, 0))
        self.mcp_status(0)
        self.mcp.stdin.close()
        require(self.mcp.wait(timeout=8) == 0)
        self.cdp.call("Browser.close")
        require(self.browser.wait(timeout=8) == 0)
        self.http.shutdown()
        self.http.server_close()
        # Existing native viewport close path is independent of exhausted Script.
        # Enumerate only top-level windows of the retained live GUI process identity.
        user32 = ctypes.WinDLL("user32", use_last_error=True)
        user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
        user32.IsWindowVisible.argtypes = [wintypes.HWND]
        user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
        windows = []
        callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
        def each(hwnd, unused):
            pid = wintypes.DWORD()
            user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
            if pid.value == self.gui.pid and user32.IsWindowVisible(hwnd):
                windows.append(hwnd)
            return True
        callback = callback_type(each)
        user32.EnumWindows.argtypes = [callback_type, wintypes.LPARAM]
        require(user32.EnumWindows(callback, 0) and len(windows) == 1 and self.gui.poll() is None)
        pid = wintypes.DWORD()
        require(user32.GetWindowThreadProcessId(windows[0], ctypes.byref(pid)) and pid.value == self.gui.pid)
        require(user32.PostMessageW(windows[0], 0x0010, 0, 0))
        require(self.gui.wait(timeout=15) == 0)
        require(not self.descriptors() and not self.setup_descriptor.exists())
        require('GUI software adapter selected: backend=Dx12, device_type=Cpu,' in (self.private / 'gui-stderr.log').read_text(encoding='utf-8', errors='replace'))
        # Names, IDs, payloads, endpoints, paths, credentials and raw logs stay private.
        self.result.update(status="PASS", same_two_gui_users_and_sources=True,
                           independent_gui_credentials=True, native_default_off=True,
                           wasm_default_off=True, native_key_down_up_observed_by_actual_wasm=True,
                           fresh_stopped_setup_status=True, native_exit_code=0, gui_exit_code=0,
                           owned_descriptor_cleanup=True, overlap_samples=samples)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--run", type=Path, required=True)
    parser.add_argument("--public-result", type=Path, required=True)
    parser.add_argument("--commit", required=True)
    args = parser.parse_args()
    pair = Pair(args)
    try:
        pair.run()
    except BaseException as error:
        # No exception text, traceback, peer data, secret-bearing expressions or logs.
        pair.result["failure_stage"] = pair.stage
        try:
            pair.result["failure_observations"] = pair.failure_observations()
        except BaseException:
            pair.result["failure_observations"] = {"schema": "pim.gui-pair.failure-observations.v1", "diagnostic_read_failed": True}
        pair.result["checkpoint"] = pair.checkpoint
        name = type(error).__name__
        allowed = {"Rejected", "KeyError", "TypeError", "ValueError", "JSONDecodeError",
                   "FileNotFoundError", "PermissionError", "TimeoutError", "TimeoutExpired",
                   "ConnectionError", "ConnectionResetError", "OSError", "Empty"}
        pair.result["failure_class"] = name if name in allowed else "UnexpectedError"
        pair.result["failure_policy"] = "preserve private partial evidence; no kill/restart/retry"
    pair.result["elapsed_ms"] = int((time.monotonic() - pair.start) * 1000)
    require(not args.public_result.exists())
    args.public_result.write_text(json.dumps(pair.result, indent=2), encoding="utf-8")
    return 0 if pair.result["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
