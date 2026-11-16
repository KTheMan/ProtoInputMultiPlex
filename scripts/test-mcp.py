"""Real MCP stdio + native backend validation, using only app-local virtual input."""
import argparse
import ctypes
from ctypes import wintypes
import json
from pathlib import Path
import queue
import shutil
import subprocess
import threading
import time


EXPECTED_TOOLS = {
    "routing_displays",
    "routing_windows",
    "routing_state",
    "routing_start",
    "routing_input",
    "routing_mode",
    "routing_stop",
    "routing_settings",
    "routing_output",
    "automation_record",
    "automation_clip",
    "automation_play",
    "automation_cancel",
    "routing_replication",
    "annotation",
    "magnifier",
    "macro_studio",
    "routing_graph",
    "mapper_library",
    "virtual_users",
    "agent_session",
    "routing_capture",
    "profile_document",
    "app_preferences",
    "application_event_status",
}


class Rect(ctypes.Structure):
    _fields_ = [(field, wintypes.LONG) for field in ("left", "top", "right", "bottom")]


user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.SetWindowPos.argtypes = [wintypes.HWND, wintypes.HWND, ctypes.c_int,
                              ctypes.c_int, ctypes.c_int, ctypes.c_int, wintypes.UINT]
user32.IsWindow.argtypes = [wintypes.HWND]


class Client:
    def __init__(self, executable, runtime, log, latest=False):
        self.stderr = open(log, "w", encoding="utf-8")
        self.process = subprocess.Popen(
            [str(executable), "mcp", "--runtime", str(runtime)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr,
            text=True, encoding="utf-8", creationflags=subprocess.CREATE_NO_WINDOW)
        self.messages = queue.Queue()
        self.next_id = 0
        self.latest = latest
        threading.Thread(target=self.read, daemon=True).start()
        if not latest:
            result = self.rpc("initialize", {"protocolVersion": "2025-11-25",
                              "capabilities": {}, "clientInfo": {"name": "virtual-routing-test", "version": "1"}})
            assert result["capabilities"]["tools"] is not None
            self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def read(self):
        try:
            for line in self.process.stdout:
                self.messages.put(json.loads(line))  # Any native stdout pollution fails here.
        except Exception as error:
            self.messages.put(error)
        finally:
            self.messages.put(EOFError("MCP stdout closed"))

    def send(self, message):
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def rpc(self, method, params):
        self.next_id += 1
        if self.latest:
            params = {**params, "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {"name": "virtual-routing-test", "version": "1"},
                "io.modelcontextprotocol/clientCapabilities": {}}}
        self.send({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params})
        deadline = time.monotonic() + 60
        while True:
            reply = self.messages.get(timeout=max(.01, deadline - time.monotonic()))
            if isinstance(reply, Exception):
                raise reply
            if reply.get("id") == self.next_id:
                assert "error" not in reply, reply
                return reply["result"]

    def tool(self, name, arguments=None, error=False):
        result = self.rpc("tools/call", {"name": name, "arguments": arguments or {}})
        assert bool(result.get("isError", False)) == error, result
        if error:
            return result
        return result.get("structuredContent") or json.loads(result["content"][0]["text"])

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            self.process.wait(timeout=12)
        assert self.process.returncode == 0, self.process.returncode
        self.stderr.close()


def eventually(operation, predicate, timeout=10):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            last = operation()
            if predicate(last):
                return last
        except (OSError, ValueError):
            pass  # Probe snapshots can be in the middle of a write.
        time.sleep(.1)
    raise AssertionError(f"Condition timed out: {last}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    args = parser.parse_args()
    runtime = args.runtime.resolve()
    run = runtime / time.strftime("mcp-%Y%m%d-%H%M%S")
    run.mkdir(parents=True)
    executable = run / "control.exe"
    shutil.copy2(args.executable, executable)
    probes = []
    client = None
    evidence = {}
    try:
        for name in ("a", "b"):
            probes.append(subprocess.Popen([str(executable), "probe", "--report", str(run / (name + ".json"))], creationflags=subprocess.CREATE_NO_WINDOW))
        client = Client(executable, runtime, run / "server.stderr.log")
        tools = client.rpc("tools/list", {})["tools"]
        assert {t["name"] for t in tools} == EXPECTED_TOOLS
        assert all(t["inputSchema"]["type"] == "object" for t in tools)
        legacy_profile = {
            "version": 1,
            "seats": [{
                "name": "Document validation",
                "mouse": "test-device-path",
                "keyboard": None,
                "target_pid": 1,
            }],
        }
        document = json.dumps(legacy_profile)
        checked = client.tool("profile_document", {"operation": "validate", "document": document})
        assert checked["valid"] and checked["seat_count"] == 1
        assert checked["legacy_ids_materialized"] and checked["stale_pid_reminder"]
        normalized = client.tool("profile_document", {"operation": "normalize", "document": document})
        normalized_profile = json.loads(normalized["json"])
        assert normalized["operation"] == "normalize"
        assert normalized_profile["seats"][0]["id"]
        exported = client.tool("profile_document", {"operation": "export", "document": normalized["json"]})
        assert json.loads(exported["json"]) == normalized_profile
        client.tool("routing_start", {"users": ["Blue"], "allowed_pids": [], "mode": "concurrent", "multi_keyboard": True}, error=True)
        pids = [p.pid for p in probes]
        inventory = eventually(lambda: client.tool("routing_windows"), lambda v: all(any(w["pid"] == p and w["title"].startswith("ProtoInputMultiPlex Routing Test") for w in v["windows"]) for p in pids), 60)
        windows = [next(w for w in inventory["windows"] if w["pid"] == pid and w["title"].startswith("ProtoInputMultiPlex Routing Test")) for pid in pids]
        for index, window in enumerate(windows):
            assert user32.SetWindowPos(window["hwnd"], wintypes.HWND(-1), 40 + index * 460, 50, 400, 360, 0x10)
        inventory = client.tool("routing_windows")
        windows = [next(w for w in inventory["windows"] if w["pid"] == pid and w["title"].startswith("ProtoInputMultiPlex Routing Test")) for pid in pids]
        points = [(w["client"]["left"] + 150, w["client"]["top"] + 140) for w in windows]
        def record(label, value):
            evidence[label] = value
            return value
        def action(kind, **fields):
            return client.tool("routing_input", {"action": kind, **fields})
        def report(name):
            return json.loads((run / (name + ".json")).read_text(encoding="utf-8"))
        record("start", client.tool("routing_start", {"users": ["Blue", "Coral"], "allowed_pids": pids, "mode": "switched", "multi_keyboard": True}))
        assert all(u["pid"] == 0 for u in evidence["start"]["users"])
        for index, point in enumerate(points):
            action("move_to", user=index, x=point[0], y=point[1])
            selected = record(f"select-{index}", action("button", user=index, button="primary", pressed=True))
            assert selected["users"][index]["pid"] == pids[index], selected
            action("button", user=index, button="primary", pressed=False)
        action("button", user=0, button="primary", pressed=True)
        blocked = record("switched-block", action("button", user=1, button="primary", pressed=True))
        assert blocked["users"][0]["root"] and not blocked["users"][1]["buttons"]
        action("key", user=1, key="w", pressed=True)
        eventually(lambda: report("b"), lambda r: r["test_keys"] == 1)
        assert report("a")["test_keys"] == 0
        action("release_all")
        record("concurrent", client.tool("routing_mode", {"mode": "concurrent", "multi_keyboard": True}))
        for index in (0, 1):
            action("button", user=index, button="primary", pressed=True)
            record(f"held-{index}", action("key", user=index, key="w", pressed=True))
        for name in ("a", "b"):
            eventually(lambda n=name: report(n), lambda r: r["buttons"] == 1 and r["test_keys"] == 1)
        before = report("b")["cursor"]
        moved = record("motion", action("motion", user=0, dx=30, dy=20))
        eventually(lambda: report("a"), lambda r: r["cursor"] == [points[0][0]+30, points[0][1]+20])
        assert report("b")["cursor"] == before
        for index, user in enumerate(moved["users"]):
            assert [user["overlay"]["x"], user["overlay"]["y"]] == report(("a", "b")[index])["cursor"]
        baseline = [report(n)["wheel_events"] for n in ("a", "b")]
        action("wheel", user=0, delta=120)
        eventually(lambda: report("a"), lambda r: r["wheel_events"] > baseline[0])
        assert report("b")["wheel_events"] == baseline[1]
        paused = record("pause", action("pause"))
        assert paused["paused"] and all(not u["buttons"] and not u["keys"] for u in paused["users"])
        record("resume", action("resume"))
        client.tool("routing_mode", {"mode": "concurrent", "multi_keyboard": False})
        action("button", user=1, button="primary", pressed=True)
        action("button", user=1, button="primary", pressed=False)
        shared = record("shared-keyboard", action("key", user=0, key="w", pressed=True))
        assert all(u["keyboard_pid"] == pids[1] for u in shared["users"])
        eventually(lambda: report("b"), lambda r: r["test_keys"] == 1)
        assert report("a")["test_keys"] == 0
        action("key", user=0, key="w", pressed=False)
        client.tool("routing_mode", {"mode": "concurrent", "multi_keyboard": True})
        client.tool("routing_input", {"action": "move_to", "user": 999, "x": 0, "y": 0}, error=True)
        client.tool("routing_start", {"users": ["Duplicate"], "allowed_pids": pids, "mode": "concurrent", "multi_keyboard": True}, error=True)
        held = record("before-eof", action("button", user=0, button="primary", pressed=True))
        action("key", user=0, key="w", pressed=True)
        client.close()  # EOF with held input must release and destroy overlays.
        client = None
        eventually(lambda: [bool(user32.IsWindow(u["overlay"]["hwnd"])) for u in held["users"]], lambda live: not any(live))
        eventually(lambda: report("a"), lambda r: r["buttons"] == 0 and r["test_keys"] == 0)
        # A fresh connection without attachment verifies explicit stop and idempotence.
        client = Client(executable, runtime, run / "second.stderr.log", latest=True)
        assert len(client.rpc("tools/list", {})["tools"]) == len(EXPECTED_TOOLS)
        client.tool("routing_start", {"users": ["Stop test"], "allowed_pids": pids, "mode": "concurrent", "multi_keyboard": True})
        stopped = record("stop", client.tool("routing_stop"))
        assert not stopped["running"] and stopped["users"] == []
        assert client.tool("routing_stop") == stopped
        client.close()
        client = None
        (run / "evidence.json").write_text(json.dumps(evidence, indent=2), encoding="utf-8")
        print(f"PASS: MCP discovery/schemas, scoped virtual input, switched arbitration, independent keyboards, concurrent holds/motion/wheel, pause/resume, invalid arguments, EOF release/overlay cleanup and explicit stop. Evidence: {run}", flush=True)
    finally:
        if client:
            try:
                client.close()
            except Exception:
                client.process.kill()
                client.process.wait()
        for process in probes:
            if process.poll() is None:
                process.terminate()
            process.wait()


if __name__ == "__main__":
    main()
