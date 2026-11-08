"""Independent finite Macro Studio gate: owned virtual x64/x86 targets only.

Requires the producer's READY/exclusive slot. Never inject physical input or poll
OS keyboard state. Recorded native messages, EDIT readback and real kernel handle
counts are independent of runtime telemetry. No production edits or build commands.
"""
import argparse
import copy
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
import json
from pathlib import Path
import time
from PIL import Image

_spec = importlib.util.spec_from_file_location("studio_base", Path(__file__).with_name("test-automation-acceptance.py"))
_base = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_base)
Gate, eventually, user32 = _base.Gate, _base.eventually, _base.user32
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
kernel32.OpenProcess.restype = wintypes.HANDLE
kernel32.GetProcessHandleCount.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
kernel32.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
user32.ClientToScreen.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.POINT)]


def event(at, action):
    return {"at_micros": at, "action": action}


def text(user, value):
    return {"type": "text", "user": user, "text": value}


def move(user, point):
    return {"type": "move_to", "user": user, "x": point[0], "y": point[1]}


def key(user, down, vk=116, scan=63):
    return {"type": "packet", "user": user, "packet": {
        "device": 0xF1000003 + user * 2, "kind": 1, "flags": 0 if down else 1,
        "x": scan, "y": 0, "data": vk}}


def button(user, down, code=0):
    return {"type": "packet", "user": user, "packet": {
        "device": 0xF1000002 + user * 2, "kind": 0,
        "flags": 1 << (code * 2 + (0 if down else 1)), "x": 0, "y": 0, "data": 0}}


def clip(*events):
    return {"version": 1, "events": list(events)}


def utf16(value):
    data = value.encode("utf-16-le")
    return [int.from_bytes(data[i:i + 2], "little") for i in range(0, len(data), 2)]


def handle_count(pid):
    handle = kernel32.OpenProcess(0x1000, False, pid)
    assert handle, ("OpenProcess owned worker", pid, ctypes.get_last_error())
    try:
        count = wintypes.DWORD()
        assert kernel32.GetProcessHandleCount(handle, ctypes.byref(count)), ctypes.get_last_error()
        return count.value
    finally:
        assert kernel32.CloseHandle(handle)


def process_alive(pid):
    handle = kernel32.OpenProcess(0x1000, False, pid)
    if not handle:
        return False
    try:
        code = wintypes.DWORD()
        assert kernel32.GetExitCodeProcess(handle, ctypes.byref(code))
        return code.value == 259
    finally:
        assert kernel32.CloseHandle(handle)


class StudioGate(Gate):
    fixture_count = 3
    run_prefix = "acceptance-macro-studio"
    tool_names = _base.TOOLS | {"macro_studio"}

    def fixture_arguments(self, index):
        return ["--capture-pattern", str(0x286098 + index * 0x10101)]

    def studio(self, operation, error=False, **fields):
        return self.client.tool("macro_studio", {"operation": operation, **fields}, error=error)

    def lane(self, user):
        return self.studio("inspect", user=user)["macro_studio"]["users"][user]

    def load(self, user, value):
        state = self.studio("load", user=user, clip=value)["macro_studio"]
        # MCP waits for a worker ACK but may drain newer periodic telemetry
        # before returning; the latest snapshot need not retain its ACK bit.
        assert state["error"] is None, state
        return state["users"][user]["clip"]

    def play(self, user, **changes):
        options = {"loops": 1, "speed": 1., "delay_ms": 0, "stop_on_escape": True, "keep_awake": False}
        options.update(changes)
        return self.studio("play", user=user, options=options)

    def cancel(self, user):
        self.studio("cancel", user=user)

    def active(self, user):
        playback = self.lane(user)["playback"]
        return playback and playback["state"] == "playing"

    def complete(self, user):
        return eventually(lambda: self.lane(user), lambda s: s["playback"] and s["playback"]["state"] == "completed", 20)

    def held(self, user, expected=True, vk=116, buttons=1):
        return eventually(lambda: self.report(user), lambda r: (vk in r["held_keys"]) == expected and (r["buttons"] & buttons) == (buttons if expected else 0))

    def baseline(self, user):
        report = self.report(user)
        assert report["native_event_count"] == len(report["native_events"]) < 4000, "finite trace budget exceeded"
        return report["native_event_count"]

    def trace(self, user, start):
        report = self.report(user)
        assert report["native_event_count"] == len(report["native_events"]), "native trace truncated"
        return report["native_events"][start:]

    def own_origin(self, user):
        point = wintypes.POINT()
        assert user32.ClientToScreen(self.report(user)["capture_hwnd"], ctypes.byref(point))
        return point.x, point.y

    def held_clip(self, user):
        return clip(event(0, key(user, True)), event(0, button(user, True)),
                    event(20_000_000, text(user, "late-program-text")))

    def discover(self):
        assert len(self.evidence["tools"]) == len(self.tool_names)
        schema = next(t["inputSchema"] for t in self.evidence["tools"] if t["name"] == "macro_studio")
        schema_text = json.dumps(schema)
        for field in ("record", "load", "edit", "generate", "reset", "play", "cancel", "delay_ms", "keep_awake", "stop_on_escape", "figure_eight"):
            assert field in schema_text, field
        for user in range(3):
            lane = self.lane(user)
            assert lane["clip"] is None and lane["loaded_events"] == 0 and not lane["recording"] and lane["power"] is None
        self.evidence["build_sha256"] = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (self.run / "control64.exe", self.run / "probe32.exe")}
        self.snapshot("current-object-schemas-independent-empty-lanes")

    def record_and_order(self):
        self.clear_all()
        self.studio("record", user=0, active=True)
        operations = [lambda: self.key(0, "f5", True), lambda: self.input("text", user=0, text="A🙂"),
                      lambda: self.input("button", user=0, button="back", pressed=True),
                      lambda: self.input("motion", user=0, dx=5, dy=3),
                      lambda: self.input("button", user=0, button="back", pressed=False),
                      lambda: self.key(0, "f5", False)]
        for operation in operations:
            count = self.baseline(0)
            operation()
            eventually(lambda: self.report(0), lambda r: r["native_event_count"] > count)
        self.input("text", user=1, text="unrecorded")
        value = self.studio("record", user=0, active=False)["macro_studio"]["users"][0]["clip"]
        assert len(value["events"]) == 6 and all(e["action"]["user"] == 0 for e in value["events"]), value
        assert [e["at_micros"] for e in value["events"]] == sorted(e["at_micros"] for e in value["events"])
        path = self.run / "recorded-user-0.json"
        path.write_text(json.dumps(value, ensure_ascii=False), encoding="utf-8")
        assert self.load(0, json.loads(path.read_text(encoding="utf-8"))) == value
        self.studio("record", user=0, active=True)
        empty = self.studio("record", user=0, active=False)["macro_studio"]["users"][0]
        assert empty["clip"] == value, "empty recording overwrote saved clip"
        self.clear_all()
        start = self.baseline(0)
        self.play(0)
        self.complete(0)
        eventually(lambda: self.report(0), lambda r: r["text"] == "A🙂" and not r["held_keys"] and not r["buttons"])
        trace = self.trace(0, start)
        significant = [(e["kind"], e["code"], e["pressed"]) for e in trace if e["kind"] != "motion"]
        assert significant == [("key", 116, True), *[("char", c, None) for c in utf16("A🙂")], ("button", 3, True), ("button", 3, False), ("key", 116, False)], trace
        down = next(i for i, e in enumerate(trace) if e["kind"] == "button" and e["pressed"])
        up = next(i for i, e in enumerate(trace) if e["kind"] == "button" and not e["pressed"])
        assert any(e["kind"] == "motion" for e in trace[down + 1:up]), trace
        assert self.report(1)["text"] == "" and self.report(2)["text"] == ""
        self.snapshot("per-user-record-save-load-empty-preservation-native-total-order")

    def authoring(self):
        value = self.load(0, clip(event(0, key(0, True)), event(200_000, text(0, "R")), event(400_000, key(0, False))))
        def edit(operation, **fields):
            return self.studio("edit", user=0, edit={"operation": operation, **fields})["macro_studio"]["users"][0]["clip"]
        value = edit("retime", index=1, at_micros=0)
        assert [e["action"]["type"] for e in value["events"][:2]] == ["packet", "text"]
        value = edit("insert", event=event(100_000, text(0, "X")))
        value = edit("replace", index=2, event=event(150_000, text(0, "Y")))
        value = edit("duplicate", start=1, end=3, offset_micros=500_000)
        value = edit("shift", start=4, end=6, delta_micros=100_000)
        assert [e["at_micros"] for e in value["events"]] == [0, 0, 150_000, 400_000, 600_000, 750_000], value
        value = edit("delete", start=4, end=6)
        assert [e["at_micros"] for e in value["events"]] == [0, 0, 150_000, 400_000]
        self.studio("edit", user=0, edit={"operation": "remap", "from": 0, "to": 1})
        assert self.lane(0)["clip"] == value
        remapped = self.lane(1)["clip"]
        assert [e["action"]["user"] for e in remapped["events"]] == [1] * 4
        self.clear_all()
        bases = [self.baseline(i) for i in range(2)]
        clocks = [self.report(i)["clock_us"] for i in range(2)]
        self.play(0, loops=1)
        self.play(1, loops=2, speed=2., delay_ms=250)
        result = [self.complete(i) for i in range(2)]
        eventually(self.texts, lambda t: t == ["RY", "RYRY", ""])
        for user, loops, duration in ((0, 1, 400_000), (1, 2, 200_000)):
            events = [e for e in self.trace(user, bases[user]) if e["kind"] == "key"]
            assert [(e["code"], e["pressed"]) for e in events] == [(116, True), (116, False)] * loops, events
            assert result[user]["playback"]["loops_completed"] == loops
            for n in range(loops):
                assert abs(events[n * 2 + 1]["at_us"] - events[n * 2]["at_us"] - duration) < 125_000, events
            if user == 1:
                assert events[0]["at_us"] - clocks[1] >= 220_000, events
        self.released()
        self.snapshot("all-editor-operations-stable-retime-copy-and-simultaneous-delayed-speed-loops")

    def cancellation_and_atomicity(self):
        self.client.tool("automation_clip", {"action": "load", "clip": self.held_clip(0)})
        for i in (0, 1):
            self.load(i, self.held_clip(i))
        self.key(2, "f7", True)
        self.input("button", user=2, button="back", pressed=True)
        for i in (0, 1):
            self.play(i, loops=None)
            self.held(i)
        original = [self.lane(i) for i in (0, 1)]
        rejected = [
            ("edit", {"user": 0, "edit": {"operation": "replace", "index": 99, "event": event(0, text(0, "bad"))}}),
            ("edit", {"user": 0, "edit": {"operation": "delete", "start": 2, "end": 1}}),
            ("edit", {"user": 0, "edit": {"operation": "shift", "start": 0, "end": 1, "delta_micros": -1}}),
            ("edit", {"user": 0, "edit": {"operation": "duplicate", "start": 0, "end": 0, "offset_micros": 1}}),
            ("edit", {"user": 0, "edit": {"operation": "remap", "from": 0, "to": 99}}),
            ("load", {"user": 0, "clip": clip(event(0, text(0, "bad\0")))}),
            ("play", {"user": 0, "options": {"loops": 0}}),
            ("play", {"user": 0, "options": {"speed": 101}}),
        ]
        for operation, fields in rejected:
            self.studio(operation, error=True, **fields)
            for i in (0, 1):
                current = self.lane(i)
                assert current["clip"] == original[i]["clip"] and current["revision"] == original[i]["revision"] and current["playback"]["state"] == "playing"
                self.held(i)
            self.held(2, vk=118, buttons=8)
        for name, fields in (("automation_record", {"recording": True}), ("automation_clip", {"action": "load", "clip": self.held_clip(0)}),
                             ("automation_play", {"loops": 1, "speed": 1.}), ("routing_replication", {"edges": []})):
            self.client.tool(name, fields, error=True)
            assert self.active(0) and self.active(1)
            for i in (0, 1):
                self.held(i)
        self.cancel(0)
        self.held(0, False)
        self.held(1)
        self.held(2, vk=118, buttons=8)
        self.play(0)
        self.held(0)
        self.input("text", user=0, text="manual")
        self.held(0, False)
        assert not self.active(0) and self.active(1)
        self.held(1)
        self.play(0)
        self.held(0)
        self.key(0, "escape", True)
        self.held(0, False)
        assert self.active(1)
        self.key(0, "escape", False)
        self.play(0, stop_on_escape=False)
        self.held(0)
        self.key(0, "escape", True)
        self.key(0, "escape", False)
        eventually(lambda: self.report(0), lambda r: [(e["vk"], e["pressed"]) for e in r["key_events"][-2:]] == [(27, True), (27, False)])
        assert self.active(0) and self.active(1)
        self.key(1, "escape", False)
        assert self.active(1), "Escape UP stopped owner"
        self.client.tool("automation_cancel")
        self.released()
        assert not self.active(0) and not self.active(1)
        self.snapshot("scoped-cancel-manual-escape-error-atomicity-and-global-cancel")
        # Reverse workflow boundary: legacy synchronized clock owns live input.
        self.client.tool("automation_clip", {"action": "load", "clip": self.held_clip(0)})
        self.client.tool("automation_play", {"loops": None, "speed": 1.})
        self.held(0)
        for operation, fields in (("play", {"user": 1, "options": {}}), ("record", {"user": 1, "active": True})):
            before = self.lane(1)
            self.studio(operation, error=True, **fields)
            after = self.lane(1)
            assert before["clip"] == after["clip"] and before["revision"] == after["revision"] and not after["recording"]
            self.held(0)
            assert self.state()["automation"]["playback"]["state"] == "playing"
        self.client.tool("automation_cancel")
        self.released()
        self.snapshot("legacy-active-rejects-studio-before-mutation")

    def replication_claims(self):
        def graph(edges):
            self.client.tool("routing_replication", {"edges": edges})
        graph([{"source": 0, "target": 1, "mouse": True, "keyboard": True, "text": False}])
        for i in (0, 1):
            self.load(i, self.held_clip(i))
        start = self.baseline(1)
        self.play(0)
        self.held(0)
        self.held(1)
        self.play(1)
        self.held(1)
        self.cancel(0)
        self.held(0, False)
        self.held(1)
        self.cancel(1)
        self.held(1, False)
        events = [(e["kind"], e["code"], e["pressed"]) for e in self.trace(1, start) if e["kind"] in ("key", "button")]
        assert events[:2] == [("key", 116, True), ("button", 0, True)] and set(events[2:]) == {("key", 116, False), ("button", 0, False)} and len(events) == 4, events
        graph([{"source": 2, "target": 1, "mouse": True, "keyboard": True, "text": False}])
        self.play(1)
        self.held(1)
        self.key(2, "f5", True)
        self.input("button", user=2, button="primary", pressed=True)
        self.cancel(1)
        self.held(1)
        self.held(2)
        self.key(2, "f5", False)
        self.input("button", user=2, button="primary", pressed=False)
        self.released()
        graph([])
        self.snapshot("replicated-studio-and-manual-shared-native-claims-last-owner-release")

    def shared_keyboard(self):
        # Explicit active-window arbitration selects B. Both distinct virtual
        # keyboards then address the same target while multi_keyboard is false.
        self.input("button", user=1, button="primary", pressed=True)
        self.input("button", user=1, button="primary", pressed=False)
        self.client.tool("routing_mode", {"mode": "concurrent", "multi_keyboard": False})
        for user in (0, 1):
            self.load(user, clip(event(0, key(user, True)), event(20_000_000, key(user, False))))
        start = self.baseline(1)
        self.play(0)
        eventually(lambda: self.report(1), lambda r: 116 in r["held_keys"])
        self.play(1)
        assert self.active(0) and self.active(1)
        self.cancel(0)
        eventually(lambda: self.report(1), lambda r: 116 in r["held_keys"])
        assert self.active(1)
        self.cancel(1)
        self.released()
        keys = [(e["code"], e["pressed"]) for e in self.trace(1, start) if e["kind"] == "key"]
        assert keys == [(116, True), (116, False)], keys
        self.client.tool("routing_mode", {"mode": "concurrent", "multi_keyboard": True})
        self.snapshot("shared-active-target-keyboard-union-first-down-last-up")

    def origin_tail(self):
        for i in (0, 1):
            self.load(i, self.held_clip(i))
            self.play(i)
            self.held(i)
        self.studio("record", user=0, active=True)
        self.held(0, False)
        self.held(1)
        assert self.active(1)
        self.input("text", user=0, text="owner recording")
        self.studio("record", user=0, active=False)
        self.held(1)
        self.cancel(1)
        for i in (0, 1):
            self.studio("record", user=i, active=True)
        for i, name, label, code in ((0, "f7", "rec-A", "back"), (1, "f6", "rec-B", "forward")):
            self.key(i, name, True)
            self.input("text", user=i, text=label)
            self.input("button", user=i, button=code, pressed=True)
        a = self.studio("record", user=0, active=False)["macro_studio"]["users"][0]["clip"]
        self.held(0, False, vk=118, buttons=8)
        self.held(1, vk=117, buttons=16)
        assert self.lane(1)["recording"]
        b = self.studio("record", user=1, active=False)["macro_studio"]["users"][1]["clip"]
        self.released()
        for i, value, label in ((0, a, "rec-A"), (1, b, "rec-B")):
            assert len(value["events"]) == 3 and [e["action"]["user"] for e in value["events"]] == [i] * 3
            assert [e["action"]["type"] for e in value["events"]] == ["packet", "text", "packet"] and value["events"][1]["action"]["text"] == label
        self.snapshot("record-start-cancels-owner-only-simultaneous-record-and-scoped-stop")
        self.client.tool("routing_replication", {"edges": [{"source": 0, "target": 1, "mouse": False, "keyboard": True, "text": False}]})
        self.load(0, clip(event(0, key(0, True)), event(100000, key(0, True, 27, 1)), event(200000, key(0, False, 27, 1)), event(2_000_000, key(0, False))))
        self.load(1, self.held_clip(1))
        self.play(1)
        self.held(1)
        start = self.baseline(1)
        self.play(0)
        eventually(lambda: [(e["code"], e["pressed"]) for e in self.trace(1, start) if e["kind"] == "key" and e["code"] == 27], lambda events: events == [(27, True), (27, False)])
        assert self.active(0) and self.active(1), "playback Escape cancelled a lane"
        self.held(1)
        self.key(0, "escape", True)
        self.key(0, "escape", False)
        assert not self.active(0) and self.active(1), "replicated manual Escape cancelled destination"
        self.held(1)
        self.cancel(1)
        self.released()
        self.client.tool("routing_replication", {"edges": []})
        self.snapshot("native-playback-and-replicated-escape-preserve-destination-program")
        self.client.tool("routing_stop")

    def annotation_tail(self):
        settings = {"tool": "brush", "activator": "primary", "color": [96, 165, 250], "alpha": 255, "width": 6, "clear_key": None, "clear_all": False}
        for i in (0, 1):
            self.client.tool("annotation", {"operation": "configure", "user": i, "settings": settings})
        self.client.tool("routing_replication", {"edges": [{"source": 0, "target": 1, "mouse": True, "keyboard": False, "text": False}]})
        counts = [self.report(i)["mouse_event_count"] for i in (0, 1)]
        self.load(1, clip(event(0, button(1, True)), event(20_000_000, button(1, False))))
        self.load(0, clip(event(0, button(0, True)), event(100_000, move(0, [self.points[0][0] + 10, self.points[0][1]])), event(20_000_000, button(0, False))))
        self.play(1, keep_awake=True)
        self.play(0)
        annotation = eventually(lambda: self.state()["annotation"], lambda a: all(a["users"][i]["open"] and a["users"][i]["segments"] >= 2 for i in (0, 1)))
        assert self.active(0) and self.active(1)
        self.cancel(0)
        cancelled = eventually(lambda: self.state()["annotation"], lambda a: not a["users"][0]["open"] and a["users"][1]["open"])
        assert self.active(1) and self.lane(1)["power"]["active"], "cancel ended unrelated drawing program/power"
        assert [self.report(i)["mouse_event_count"] for i in (0, 1)] == counts, "consumed drawing activator reached native EDIT"
        self.cancel(1)
        eventually(lambda: self.state()["annotation"], lambda a: not any(u["open"] for u in a["users"]))
        self.released()
        path = self.run / "studio-claims-ink.png"
        exported = self.client.tool("annotation", {"operation": "export", "user": 0, "kind": "ink_only", "path": str(path)})["annotation"]
        bounds = exported["overlay"]["bounds"]
        with Image.open(path) as image:
            assert image.mode == "RGBA"
            for i in (0, 1):
                assert image.getpixel((self.points[i][0] + 5 - bounds["left"], self.points[i][1] - bounds["top"])) == (96, 165, 250, 255)
            png = {"path": str(path), "size": list(image.size), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
        self.evidence["annotation_claims"] = {"during": annotation, "cancelled_source": cancelled, "native_counts_before": counts, "png": png}
        self.snapshot("actual-ink-consumed-replicated-activator-origin-cancel-preserves-other-open-stroke")
        self.client.tool("routing_stop")

    def generators(self):
        cx, cy = self.points[0]
        shapes = {
            "circle": [(12, 0), (0, 12), (-12, 0), (0, -12), (12, 0)],
            "square": [(-12, -12), (12, -12), (12, 12), (-12, 12), (-12, -12)],
            "triangle": [(0, -12), (10, 6), (-10, 6), (0, -12)],
            "line": [(-12, 0), (12, 0), (-12, 0)],
            "figure_eight": [(0, 0), (8, 6), (12, 0), (8, -6), (0, 0), (-8, 6), (-12, 0), (-8, -6), (0, 0)]}
        def generate(shape, points, click=None, seed=41):
            return self.studio("generate", user=0, settings={"user": 0, "shape": shape, "center": [cx, cy], "radius": 12,
                "points": points, "duration_micros": 360_000, "click": click, "seed": seed})["macro_studio"]["users"][0]["clip"]
        def native_points(start):
            ox, oy = self.own_origin(0)
            out = []
            for e in self.trace(0, start):
                if e["kind"] == "motion":
                    point = [e["client"][0] + ox, e["client"][1] + oy]
                    if not out or point != out[-1]:
                        out.append(point)
            return out
        for shape, relative in shapes.items():
            self.input("move_to", user=0, x=cx + 25, y=cy + 25)
            eventually(lambda: self.report(0), lambda r: r["cursor"] == [cx + 25, cy + 25])
            value = generate(shape, len(relative) - 1)
            expected = [[cx + x, cy + y] for x, y in relative]
            assert [[e["action"]["x"], e["action"]["y"]] for e in value["events"]] == expected, value
            assert [e["at_micros"] for e in value["events"]] == [360_000 * i // (len(relative) - 1) for i in range(len(relative))]
            start = self.baseline(0)
            self.play(0)
            self.complete(0)
            eventually(lambda: native_points(start), lambda p: p == expected)
            self.snapshot("native-generated-" + shape)
        value = generate("jiggle", 6)
        assert generate("jiggle", 6) == value and generate("jiggle", 6, seed=42) != value
        assert len(value["events"]) == 6 and all(e["action"]["type"] == "move_to" for e in value["events"])
        expected = [[e["action"]["x"], e["action"]["y"]] for e in value["events"]]
        assert all(abs(x - cx) <= 12 and abs(y - cy) <= 12 for x, y in expected)
        self.load(0, value)
        start = self.baseline(0)
        self.play(0)
        self.complete(0)
        eventually(lambda: native_points(start), lambda p: p == expected)
        self.snapshot("seeded-jiggle-reproducible-native-movement-only")
        for code in range(5):
            value = generate("line", 2, click=code)
            assert len(value["events"]) == 9
            for i in range(3):
                group = value["events"][i * 3:(i + 1) * 3]
                assert len({e["at_micros"] for e in group}) == 1
                assert [e["action"]["type"] for e in group] == ["move_to", "packet", "packet"]
                assert [e["action"]["packet"]["flags"] for e in group[1:]] == [1 << (2 * code), 2 << (2 * code)]
            start = self.baseline(0)
            self.play(0)
            self.complete(0)
            eventually(lambda: [e for e in self.trace(0, start) if e["kind"] == "button"], lambda es: len(es) == 6)
            assert [(e["code"], e["pressed"]) for e in self.trace(0, start) if e["kind"] == "button"] == [(code, True), (code, False)] * 3
            self.released()
            self.snapshot("native-generator-paired-button-" + str(code))
        preserved = self.lane(0)["clip"]
        for settings in ({"shape": "jiggle", "click": 0}, {"shape": "circle", "points": 2}, {"duration_micros": 0}, {"radius": 1_000_001}):
            self.studio("generate", user=0, settings={"user": 0, **settings}, error=True)
            assert self.lane(0)["clip"] == preserved
        self.studio("edit", user=0, edit={"operation": "clear"})
        assert self.lane(0)["clip"] is None
        self.studio("reset", user=1)
        assert self.lane(1)["clip"] is None
        self.snapshot("invalid-generator-preserves-clip-clear-and-reset")

    def power_and_lifecycle(self, eof=False):
        for i in (0, 1):
            self.load(i, self.held_clip(i))
        # Warm the actual OS request path, then measure the same worker at rest.
        self.play(0, keep_awake=True)
        power = self.lane(0)["power"]
        assert power and power["active"] and power["display_required"] and power["system_required"]
        pid = power["process_id"]
        assert pid not in [p.pid for p in self.probes] and pid != self.client.process.pid
        self.cancel(0)
        self.released()
        baseline = handle_count(pid)
        self.play(0, keep_awake=False)
        self.held(0)
        assert self.lane(0)["power"] is None and handle_count(pid) == baseline
        self.cancel(0)
        counts = []
        for i in (0, 1):
            self.play(i, keep_awake=True)
            self.held(i)
            status = self.lane(i)["power"]
            assert status["process_id"] == pid and status["active"] and status["display_required"] and status["system_required"]
            counts.append(handle_count(pid))
            assert counts[-1] == baseline + i + 1, (baseline, counts)
        before = self.lane(0)
        self.studio("play", user=0, options={"speed": 0}, error=True)
        assert handle_count(pid) == baseline + 2 and self.lane(0)["power"] == before["power"]
        self.cancel(0)
        self.held(0, False)
        self.held(1)
        assert handle_count(pid) == baseline + 1 and self.lane(0)["power"] is None
        self.cancel(1)
        self.released()
        assert handle_count(pid) == baseline
        self.load(0, clip(event(0, key(0, True)), event(250_000, key(0, False))))
        self.play(0, keep_awake=True)
        self.complete(0)
        self.released()
        assert self.lane(0)["power"] is None and handle_count(pid) == baseline
        self.load(0, self.held_clip(0))
        for i in (0, 1):
            self.play(i, keep_awake=True)
            self.held(i)
        self.input("pause")
        self.released()
        assert all(self.lane(i)["power"] is None and not self.active(i) for i in (0, 1)) and handle_count(pid) == baseline
        self.input("resume")
        self.evidence["power"] = {"owned_worker_pid": pid, "baseline_kernel_handles": baseline, "two_active_counts": counts,
            "proof": "Actual PowerCreateRequest/PowerSetRequest-backed status and one kernel handle per active guard; cancellation, completion and pause return to baseline.",
            "limitation": "Unprivileged powercfg /requests unavailable. No forced OS power API failure or actual idle/sleep override test."}
        self.snapshot("actual-keep-awake-handle-acquisition-scoped-cancel-completion-pause-cleanup")
        for i in (0, 1):
            self.play(i, keep_awake=True)
            self.held(i)
        overlays = [u["overlay"]["hwnd"] for u in self.state()["users"]]
        if eof:
            self.client.close()
            self.client = None
            self.released()
            eventually(lambda: process_alive(pid), lambda alive: not alive)
        else:
            self.client.tool("routing_stop")
            self.released()
            assert self.state()["macro_studio"] is None and not self.state()["running"]
            eventually(lambda: process_alive(pid), lambda alive: not alive)
        eventually(lambda: [bool(user32.IsWindow(h)) for h in overlays], lambda alive: not any(alive))
        self.evidence["checks"]["eof-native-held-and-power-process-cleanup" if eof else "stop-native-held-overlay-and-power-cleanup"] = {"probes": self.reports(), "worker_pid": pid, "overlay_hwnds": overlays}
        self.save()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--eof-only", action="store_true")
    parser.add_argument("--origin-tail", action="store_true")
    parser.add_argument("--annotation-tail", action="store_true")
    args = parser.parse_args()
    gate = StudioGate(args)
    try:
        gate.start()
        gate.discover()
        if args.annotation_tail:
            gate.annotation_tail()
        elif args.origin_tail:
            gate.origin_tail()
        elif not args.eof_only:
            gate.record_and_order()
            gate.authoring()
            gate.cancellation_and_atomicity()
            gate.replication_claims()
            gate.shared_keyboard()
            gate.generators()
        if not args.origin_tail and not args.annotation_tail:
            gate.power_and_lifecycle(eof=args.eof_only)
        gate.evidence["result"] = "PASS"
        print(f"PASS: finite Macro Studio native gate. Evidence: {gate.run}", flush=True)
    except Exception as error:
        gate.evidence["result"] = "NOT ACCEPTED"
        gate.evidence["failure"] = repr(error)
        print(f"NOT ACCEPTED: {error!a}. Evidence: {gate.run}", flush=True)
        raise
    finally:
        gate.close()


if __name__ == "__main__":
    main()
