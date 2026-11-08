"""Disposable hosted GUI/native/Web/MCP owned-window acceptance.

Private descriptors, epoch-bound references, DevTools payloads and raw native
reports stay in the runner's owned directory. Only fixed decisions are public.
The workflow owns an independent deadline and never kills or replays children.
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
import subprocess
import threading
import time
from urllib.parse import urlsplit


spec = importlib.util.spec_from_file_location("pim_gui_pair_base", Path(__file__).with_name("qualify-gui-pair.py"))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
require = base.require


def clipboard_text():
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    user32.OpenClipboard.argtypes = [wintypes.HWND]
    user32.OpenClipboard.restype = wintypes.BOOL
    user32.GetClipboardData.argtypes = [wintypes.UINT]
    user32.GetClipboardData.restype = wintypes.HANDLE
    user32.CloseClipboard.restype = wintypes.BOOL
    kernel32.GlobalSize.argtypes = [wintypes.HANDLE]
    kernel32.GlobalSize.restype = ctypes.c_size_t
    kernel32.GlobalLock.argtypes = [wintypes.HANDLE]
    kernel32.GlobalLock.restype = ctypes.c_void_p
    kernel32.GlobalUnlock.argtypes = [wintypes.HANDLE]
    kernel32.GlobalUnlock.restype = wintypes.BOOL
    require(user32.OpenClipboard(None))
    try:
        handle = user32.GetClipboardData(13)  # CF_UNICODETEXT, GUI Copy action only.
        require(bool(handle))
        size = kernel32.GlobalSize(handle)
        require(2 <= size <= 512 and size % 2 == 0)
        pointer = kernel32.GlobalLock(handle)
        require(bool(pointer))
        try:
            value = ctypes.wstring_at(pointer, size // 2).split("\0", 1)[0]
            require(len(value) <= 256)
            return value
        finally:
            kernel32.GlobalUnlock(handle)
    finally:
        user32.CloseClipboard()


class WindowPair(base.Pair):
    def __init__(self, args):
        super().__init__(args)
        self.end = self.start + 220
        self.result = {
            "schema": "pim.gui-owned-window.hosted.v1", "source_commit": args.commit,
            "status": "FAIL", "scope": "one real GUI-published native window",
            "full_sdk_complete": False, "release_qualified": False,
            "hardware_qualified": False, "positive_owner_actions": 0,
            "uncertain_late_action": False, "old_epoch_rejected": False,
            "host_pending_expired": False,
            "post_revoke_rejected": False, "rust_mcp_fixture_invoked": False,
        }
        self.expected_timeout = False

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
            require(value.get("queue_overflows") == 0)
            require(value.get("uncertain_admissions") == (1 if self.expected_timeout else 0))
            require(value.get("window_uncertain") == (1 if self.expected_timeout else 0))
        return value

    def issue_window_descriptor(self, count):
        paths = self.descriptors()
        if len(paths) != count:
            return None
        new = paths - self.issued
        require(len(new) == 1)
        path = new.pop()
        value = self.strict_json("authority_descriptor", path)
        require(set(value) == {"protoinputmultiplex_application_events", "endpoint", "exact_origin", "credential", "expires_unix_ms"})
        require(value["protoinputmultiplex_application_events"] == 1 and value["exact_origin"] == "http://127.0.0.1:4173")
        endpoint = urlsplit(value["endpoint"])
        require(endpoint.scheme == "ws" and endpoint.hostname == "127.0.0.1" and endpoint.port and endpoint.path == "/application-events/v1" and not (endpoint.query or endpoint.fragment or endpoint.username or endpoint.password))
        require(re.fullmatch("[0-9a-fA-F]{64}", value["credential"]) is not None)
        require(250000 < value["expires_unix_ms"] - time.time() * 1000 <= 301000)
        match = re.fullmatch(rf"protoinputmultiplex\.application-events\.{self.gui.pid}\.(\d+)\.(\d+)\.json", path.name)
        require(match is not None)
        epoch = int(match[1])
        if count == 1:
            self.epoch = epoch
            self.endpoint = value["endpoint"]
            self.credentials = set()
        require(epoch == self.epoch and value["endpoint"] == self.endpoint and value["credential"] not in self.credentials)
        self.credentials.add(value["credential"])
        self.issued = paths
        self.connection_paths[count] = path
        return True

    def observe_publication(self):
        published = self.native_detail("native_window_published")
        changed = self.native_detail("native_window_changed")
        moved = self.native_detail("native_window_moved")
        status = self.event_status("active", 0, 1, 1)
        if not (published and changed and moved and status and status.get("published_windows") == 1):
            return None
        require(published == {"identity_metadata_consent": False, "source": "native_client_reported", "users_required": False})
        require(changed["actual_title_change"] and changed["metadata_identity_absent"])
        require(moved["actual_os_move"] and moved["os_bounds"] == [150, 120, 550, 420])
        require(not status["pointer_export"] and not status["keyboard_export"])
        return True

    def browser_start(self):
        edge = Path(os.environ["ProgramFiles(x86)"]) / "Microsoft/Edge/Application/msedge.exe"
        require(edge.is_file())
        profile = self.private / "edge-profile"
        self.browser = self.launch([edge, "--headless=new", "--window-size=1280,1000", "--disable-gpu", "--disable-extensions", "--no-first-run", "--no-default-browser-check", "--remote-debugging-address=127.0.0.1", "--remote-debugging-port=0", "--user-data-dir=" + str(profile), "about:blank"], "edge")
        def devtools():
            raw, _ = self.live_bytes("browser_devtools", profile / "DevToolsActivePort", 4096)
            lines = raw.decode("utf-8-sig").splitlines()
            return lines if len(lines) == 2 and lines[0].isdigit() else None
        lines = self.wait(devtools)
        self.cdp = base.CDP("ws://127.0.0.1:" + lines[0] + lines[1])
        target = self.cdp.call("Target.createTarget", {"url": "http://127.0.0.1:4173/window.html"})["targetId"]
        self.session = self.cdp.call("Target.attachToTarget", {"targetId": target, "flatten": True})["sessionId"]
        self.wait(lambda: self.web("!!window.pimClient && !!window.runWindowRouteAcceptance"))
        descriptor = self.strict_json("authority_descriptor", self.connection_paths[2])
        self.web("window.pimClient.connect(" + json.dumps(descriptor["endpoint"]) + "," + json.dumps(descriptor["credential"]) + "); true")
        del descriptor
        state = self.wait(self.state)
        require(state["epoch"] == self.epoch and state["users"] == [])
        require(state["event_grant"]["owned_windows"] and state["control_grant"]["window_tracking"] and state["control_grant"]["window_control"])
        require(not state["control_grant"].get("window_publication", False) and not state["event_grant"]["pointer_events"] and not state["event_grant"]["key_events"])
        require(state["windows"] == [])
        self.wait(lambda: self.event_status("active", 0, 2, 2))

    def browser_acceptance(self):
        self.web("window.pimGate=null; window.runWindowRouteAcceptance(window.pimClient," + json.dumps(self.reference_json) + ").then(x=>window.pimGate=x,()=>window.pimGate={status:'FAIL'}); true")
        result = self.wait(lambda: self.web("window.pimGate"), 25)
        require(result == {"status": "PASS", "query": True, "track": True, "owner_action": True, "untrack": True})
        self.wait(lambda: len([x for x in self.native_rows() if x["stage"] == "native_window_action_executed"]) >= 1)
        self.result["web_query_track_control_untrack"] = True

    def rpc_start(self):
        err = open(self.private / "mcp-direct-stderr.log", "xb")
        self.mcp = subprocess.Popen([str(self.exe), "mcp", "--event-connection", str(self.connection_paths[4])], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err, text=True, encoding="utf-8", creationflags=subprocess.CREATE_NO_WINDOW)
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
        self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "pim-window-gate", "version": "1"}})
        self.mcp.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
        self.mcp.stdin.flush()

    def tool(self, operation, **args):
        reply = self.rpc("tools/call", {"name": "native_windows", "arguments": {"operation": operation, **args}})
        return reply, reply.get("structuredContent")

    def mcp_acceptance(self):
        self.rpc_start()
        reply, value = self.tool("inspect")
        require(not reply.get("isError") and value["epoch"] == self.epoch)
        stale_epoch = self.epoch - 1 if self.epoch > 1 else 2
        stale, _ = self.tool("query", epoch=stale_epoch, window_id=self.reference["window"])
        require(stale.get("isError") is True and "stale session" in stale["content"][0]["text"].lower())
        self.result["old_epoch_rejected"] = True
        reply, value = self.tool("query", epoch=self.epoch, window_id=self.reference["window"])
        require(not reply.get("isError") and value["window"]["id"] == self.reference["window"])
        require(value["window"]["observation"]["source"] == "native_client_reported" and value["window"]["metadata"]["title"] == "Owned SDK fixture window")
        moved = self.native_detail("native_window_moved")
        require([value["window"]["bounds"][key] for key in ("left", "top", "right", "bottom")] == moved["os_bounds"])
        reply, value = self.tool("track", epoch=self.epoch, window_id=self.reference["window"])
        require(not reply.get("isError") and value["window"]["id"] == self.reference["window"])
        reply, value = self.tool("refresh")
        require(not reply.get("isError") and any(row["id"] == self.reference["window"] for row in value["windows"]))
        bounds = {"left": 250, "top": 200, "right": 650, "bottom": 550}
        reply, value = self.tool("control", epoch=self.epoch, window_id=self.reference["window"], control={"kind": "move_resize", "value": bounds})
        require(not reply.get("isError") and value["acknowledgement"]["window"] == self.reference["window"])
        require(value["acknowledgement"]["action"] == "move_resize" and value["acknowledgement"]["observed"]["bounds"] == bounds)
        self.wait(lambda: len([x for x in self.native_rows() if x["stage"] == "native_window_action_executed"]) >= 3)
        actions = [x["detail"] for x in self.native_rows() if x["stage"] == "native_window_action_executed"]
        require(len(actions) == 3 and actions[0]["control"] == {"kind": "restore"} and actions[1]["control"] == {"kind": "restore"} and actions[2]["control"] == {"kind": "move_resize", "value": bounds})
        require(actions[2]["os_bounds"] == [bounds["left"], bounds["top"], bounds["right"], bounds["bottom"]] and actions[2]["os_visible"])
        self.result["positive_owner_actions"] = 3
        self.wait(lambda: self.native_detail("native_window_timeout_ready"))
        sent_at = time.monotonic()
        reply, _ = self.tool("control", epoch=self.epoch, window_id=self.reference["window"], control={"kind": "restore"})
        elapsed = time.monotonic() - sent_at
        require(reply.get("isError") is True and "window action outcome uncertain" in reply["content"][0]["text"].lower() and elapsed < 5.5)
        reply, value = self.tool("inspect")
        require(not reply.get("isError") and value["available"] is True and value["needs_refresh"] is True and value["windows"] == [])
        self.result["host_pending_expired"] = True
        # An outward uncertain result alone is insufficient: the native owner
        # must record the late fourth OS action after the paused message loop.
        late = self.wait(lambda: self.native_detail("native_window_timeout_late_action"), 15)
        require(late["fourth_action_executed_after_owner_pause"] and late["actions"] == 4 and late["late_receipt_rejected"])
        actions = [x["detail"] for x in self.native_rows() if x["stage"] == "native_window_action_executed"]
        require(len(actions) == 4 and actions[3]["control"] == {"kind": "restore"})
        self.expected_timeout = True
        self.result["uncertain_late_action"] = True

    def sample(self, step):
        if step == 6:
            value = self.event_status("local", 0, 0)
            return value and not value["pointer_export"] and not value["keyboard_export"]
        if step == 17:
            return self.event_status("active", 0, 1, 0)
        if step == 18:
            if not self.issue_window_descriptor(1):
                return None
            self.native_report = self.private / "native-window.jsonl"
            self.native = self.launch([self.native_exe, "--dll", self.dll, "--sha256", self.native_meta["dll"]["sha256"], "--descriptor", self.connection_paths[1], "--report", self.native_report, "--window-route", "--observe-seconds", "60"], "native-window")
            self.wait(self.observe_publication, 15)
            return True
        if step == 24:
            try:
                raw = clipboard_text()
                value = json.loads(raw)
            except (base.Rejected, json.JSONDecodeError, OSError):
                return None
            if not (isinstance(value, dict) and set(value) == {"epoch", "window"} and type(value["window"]) is int and value["window"] > 0 and value["epoch"] == self.epoch):
                return None
            self.reference, self.reference_json = value, raw
            return True
        if step == 26:
            if not self.issue_window_descriptor(2):
                return None
            self.browser_start()
            return True
        if step == 33:
            return self.issue_window_descriptor(3)
        if step == 40:
            if not self.issue_window_descriptor(4):
                return None
            self.browser_acceptance()
            self.run_rust_mcp_fixture()
            self.mcp_acceptance()
            return True
        if step == 42:
            self.wait(lambda: (value if value and value.get("published_windows") == 1 else None)
                      if (value := self.event_status("active", 0, 2, 2)) else None)
            self.wait(lambda: self.state() is None)
            rejected = self.web("(()=>{try{window.pimClient.query_window(" + json.dumps(self.reference_json) + ");return false}catch(_){return true}})()")
            require(rejected is True)
            self.result["post_revoke_rejected"] = True
            return True
        raise base.Rejected()

    def run_rust_mcp_fixture(self):
        env = dict(os.environ, PIM_EVENT_WINDOW_APPROVED="1", PIM_TEST_SERVER=str(self.exe),
                   PIM_EVENT_CONNECTION=str(self.connection_paths[3]),
                   PIM_EVENT_WINDOW_EPOCH=str(self.epoch), PIM_EVENT_WINDOW_ID=str(self.reference["window"]))
        self.fixture = self.launch([self.args.mcp_fixture, "--ignored", "--exact", "real_native_window_agent_query_track_control_untrack"], "rust-mcp-window", env)
        self.result["rust_mcp_fixture_invoked"] = True
        self.wait(lambda: self.fixture.poll() is not None, 20)
        require(self.fixture.poll() == 0)
        self.wait(lambda: len([x for x in self.native_rows() if x["stage"] == "native_window_action_executed"]) >= 2)

    def run(self):
        a = self.args
        require(os.name == "nt" and os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted")
        require(re.fullmatch("[0-9a-f]{40}", a.commit) is not None and a.mcp_fixture.is_file())
        self.private = a.run.resolve()
        require(not self.private.exists())
        self.private.mkdir()
        self.ui, self.temp = self.private / "ui", self.private / "setup-temp"
        self.ui.mkdir(); self.temp.mkdir()
        prefs = self.private / "preferences"
        prefs.mkdir()
        self.preferences = prefs / "app-preferences.json"
        shutil.copyfile(a.source / "fixtures/gui-sdk-preparation/app-preferences.json", self.preferences)
        self.steps = self.strict_json("prefix_json", a.source / "fixtures/gui-sdk-owned-window/prepare.json")
        require(len(self.steps) == 42)
        shutil.copyfile(a.source / "fixtures/gui-sdk-owned-window/prepare.json", self.ui / "prepare.json")
        gui_root, native_root = a.artifacts / "sdk-preview/gui", a.artifacts / "sdk-preview/x64"
        gui_meta = self.strict_json("artifact_json", gui_root / "metadata.json")
        self.native_meta = self.strict_json("artifact_json", native_root / "metadata.json")
        require(gui_meta["source_commit"] == self.native_meta["source_commit"] == a.commit and self.native_meta["native_client_abi_version"] == 3)
        self.exe = self.checked(gui_root, gui_meta["executable"], "protoinputmultiplex.exe")
        self.native_exe = self.checked(native_root, self.native_meta["external_fixture"], "native_external_consumer.exe")
        self.dll = self.checked(native_root, self.native_meta["dll"], "mux_application_sdk.dll")
        web_root = a.artifacts / "application-web"
        web_meta = self.strict_json("artifact_json", web_root / "metadata.json")
        require(web_meta["source_commit"] == a.commit and web_meta["rust_wasm"] is True)
        require({x["file"] for x in web_meta["files"]} == {"index.html", "bootstrap.js", "pkg/mux_application_web.js", "pkg/mux_application_web_bg.wasm"})
        public = self.private / "static"
        public.mkdir()
        (public / "pkg").mkdir()
        for entry in web_meta["files"]:
            source = self.checked(web_root, entry, entry["file"])
            target = public / entry["file"]
            shutil.copyfile(source, target)
        shutil.copyfile(a.source / "crates/mux-application-web/example/window_route_acceptance.js", public / "window_route_acceptance.js")
        (public / "window.js").write_text("import {client} from './bootstrap.js'; import {runWindowRouteAcceptance} from './window_route_acceptance.js'; window.pimClient=client; window.runWindowRouteAcceptance=runWindowRouteAcceptance;", encoding="utf-8")
        (public / "window.html").write_text('<!doctype html><div id="application-demo"></div><script type="module" src="./window.js"></script>', encoding="utf-8")
        class Handler(http.server.SimpleHTTPRequestHandler):
            def __init__(self, *args, **kwargs):
                super().__init__(*args, directory=str(public), **kwargs)
            def log_message(self, *args):
                pass
        self.http = base.StaticServer(("127.0.0.1", 4173), Handler)
        threading.Thread(target=self.http.serve_forever, daemon=True).start()
        self.stage = "gui_preparation"
        env = dict(os.environ, TEMP=str(self.temp), TMP=str(self.temp))
        self.gui = self.launch([self.exe, "gui", "--renderer", "software-dx12", "--preferences", self.preferences, "--ui-script", self.ui / "prepare.json", "--ui-script-no-screenshots"], "gui", env)
        self.issued, self.connection_paths = set(), {}
        for index, step in enumerate(self.steps, 1):
            self.checkpoint = index
            self.wait(lambda: len(self.receipts()) >= index)
            rows = self.receipts()
            require(len(rows) == index or not step.get("wait_for_sample"))
            require(rows[index - 1]["button"] == step["button"] and rows[index - 1]["clicked"] is True)
            if step.get("wait_for_sample"):
                self.wait(lambda: self.sample(index), 40)
                self.acknowledge(index)
        self.stage = "native_retirement"
        self.wait(lambda: self.native.poll() is not None, 15)
        require(self.native.poll() == 0)
        retired = self.native_detail("native_window_retired")
        require(retired["actions"] == 4 and retired["owner_receipts"] == 3 and retired["late_rejections"] == 1 and retired["retired_before_hwnd_destroy"])
        require(self.native_detail("dll_unloaded_before_owner_exit") and self.native_detail("owner_ready_to_exit_after_unload"))
        self.wait(lambda: (value if value and value.get("published_windows") == 0 and value.get("window_pending") == 0 else None)
                  if (value := self.event_status("active", 0, 1, 1)) else None)
        self.mcp.stdin.close()
        require(self.mcp.wait(timeout=8) == 0)
        self.wait(lambda: (value if value and value.get("published_windows") == 0 and value.get("window_pending") == 0 else None)
                  if (value := self.event_status("active", 0, 0, 0)) else None)
        self.cdp.call("Browser.close")
        require(self.browser.wait(timeout=8) == 0)
        self.http.shutdown(); self.http.server_close()
        self.close_gui_owned_main()
        require(self.gui.wait(timeout=15) == 0 and not self.descriptors())
        require(self.result["positive_owner_actions"] == 3 and self.result["host_pending_expired"] and self.result["uncertain_late_action"] and self.result["old_epoch_rejected"] and self.result["post_revoke_rejected"] and self.result["rust_mcp_fixture_invoked"] and self.result["web_query_track_control_untrack"])
        self.result.update(status="PASS", native_exit_code=0, gui_exit_code=0, owned_descriptor_cleanup=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--mcp-fixture", type=Path, required=True)
    parser.add_argument("--run", type=Path, required=True)
    parser.add_argument("--public-result", type=Path, required=True)
    parser.add_argument("--commit", required=True)
    args = parser.parse_args()
    pair = WindowPair(args)
    try:
        pair.run()
    except BaseException as error:
        pair.result["failure_stage"] = pair.stage
        pair.result["checkpoint"] = pair.checkpoint
        pair.result["failure_class"] = type(error).__name__ if type(error).__name__ in {"Rejected", "KeyError", "TypeError", "ValueError", "JSONDecodeError", "FileNotFoundError", "PermissionError", "TimeoutError", "TimeoutExpired", "ConnectionError", "ConnectionResetError", "OSError", "Empty"} else "UnexpectedError"
        pair.result["failure_policy"] = "private partial evidence preserved; no kill, restart, or replay"
    pair.result["elapsed_ms"] = int((time.monotonic() - pair.start) * 1000)
    require(not args.public_result.exists())
    args.public_result.write_text(json.dumps(pair.result, indent=2), encoding="utf-8")
    return 0 if pair.result["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
