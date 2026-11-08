"""Independent finite native acceptance gate: virtual input, owned x64/x86 EDIT fixtures.

No hardware input, RustDesk, SendInput or SetCursorPos. Never run concurrently with
another routing session. Evidence includes actual EDIT readback and window messages.
"""
import argparse
import ctypes
from ctypes import wintypes
import json
import importlib.util
from pathlib import Path
import shutil
import struct
import subprocess
import time

_client_spec = importlib.util.spec_from_file_location("mcp_acceptance_client", Path(__file__).with_name("test-mcp.py"))
_client_module = importlib.util.module_from_spec(_client_spec)
_client_spec.loader.exec_module(_client_module)
Client, eventually = _client_module.Client, _client_module.eventually


user32 = ctypes.WinDLL("user32", use_last_error=True)
user32.SetWindowPos.argtypes = [wintypes.HWND, wintypes.HWND, ctypes.c_int,
                              ctypes.c_int, ctypes.c_int, ctypes.c_int, wintypes.UINT]
user32.IsWindow.argtypes = [wintypes.HWND]
PREFIX = "ProtoInputMultiPlex Automation Test"
TOOLS = {"routing_displays", "routing_windows", "routing_state", "routing_start", "routing_input",
         "routing_mode", "routing_stop", "routing_settings", "automation_record", "automation_clip",
         "automation_play", "automation_cancel", "routing_replication", "annotation", "magnifier",
         "macro_studio", "routing_graph"}


class Gate:
    fixture_count = 4
    tool_names = TOOLS
    run_prefix = "acceptance-automation"

    def __init__(self, args):
        self.args = args
        self.run = args.runtime.resolve() / time.strftime(self.run_prefix + "-%Y%m%d-%H%M%S")
        self.run.mkdir(parents=True)
        self.probes = []
        self.client = None
        self.evidence = {"checks": {}, "architectures": ["x64", "x86", "x64", "x86"][:self.fixture_count]}

    def report(self, index):
        return json.loads((self.run / f"probe-{index}.json").read_text(encoding="utf-8"))

    def reports(self):
        return [self.report(i) for i in range(self.fixture_count)]

    def state(self):
        return self.client.tool("routing_state")

    def snapshot(self, name):
        self.evidence["checks"][name] = {"state": self.state(), "probes": self.reports()}
        self.save()

    def save(self):
        (self.run / "evidence.json").write_text(json.dumps(self.evidence, indent=2, ensure_ascii=False), encoding="utf-8")

    def input(self, action, **fields):
        return self.client.tool("routing_input", {"action": action, **fields})

    def key(self, user, key, pressed):
        return self.input("key", user=user, key=key, pressed=pressed)

    def tap(self, user, key):
        self.key(user, key, True)
        self.key(user, key, False)

    def texts(self):
        return [r["text"] for r in self.reports()]

    def expect_texts(self, expected):
        eventually(self.texts, lambda actual: actual == expected)

    def clear_text(self, user):
        # Standard EDIT does not promise Ctrl+A. Exercise its supported navigation
        # and selection chords, waiting for window delivery before releasing modifiers.
        def delivered_tap(key):
            count = self.report(user)["key_event_count"]
            self.key(user, key, True)
            eventually(lambda: self.report(user), lambda r: r["key_event_count"] >= count + 1)
            self.key(user, key, False)
            eventually(lambda: self.report(user), lambda r: r["key_event_count"] >= count + 2)
        self.key(user, "leftctrl", True)
        eventually(lambda: self.report(user), lambda r: 162 in r["held_keys"])
        delivered_tap("home")
        self.key(user, "leftctrl", False)
        eventually(lambda: self.report(user), lambda r: 162 not in r["held_keys"])
        self.key(user, "leftshift", True)
        eventually(lambda: self.report(user), lambda r: 160 in r["held_keys"])
        delivered_tap("end")
        self.key(user, "leftshift", False)
        eventually(lambda: self.report(user), lambda r: 160 not in r["held_keys"])
        delivered_tap("delete")
        eventually(lambda: self.report(user), lambda r: r["text"] == "")

    def clear_all(self):
        for user in range(self.fixture_count):
            self.clear_text(user)

    def released(self):
        eventually(self.reports, lambda rs: all(not r["held_keys"] and not r["buttons"] for r in rs))

    def fixture_arguments(self, index):
        return []

    def before_selection(self):
        pass

    def start(self):
        for path, expected in ((self.args.executable, 0x8664), (self.args.x86_executable, 0x14C)):
            with path.open("rb") as image:
                assert image.read(2) == b"MZ", path
                image.seek(0x3C)
                image.seek(struct.unpack("<I", image.read(4))[0])
                assert image.read(4) == b"PE\0\0", path
                assert struct.unpack("<H", image.read(2))[0] == expected, "fixture architecture mismatch"
        images = []
        for name, source in (("control64.exe", self.args.executable), ("probe32.exe", self.args.x86_executable)):
            image = self.run / name
            shutil.copy2(source, image)
            images.append(image)
        for index in range(self.fixture_count):
            self.probes.append(subprocess.Popen(
                [str(images[index % 2]), "probe", "--automation", "--report", str(self.run / f"probe-{index}.json"), *self.fixture_arguments(index)],
                creationflags=subprocess.CREATE_NO_WINDOW))
        self.client = Client(images[0], self.args.runtime.resolve(), self.run / "mcp.stderr.log")
        tools = self.client.rpc("tools/list", {})["tools"]
        assert {tool["name"] for tool in tools} == self.tool_names, tools
        assert all(tool["inputSchema"]["type"] == "object" for tool in tools)
        self.evidence["tools"] = tools
        pids = [p.pid for p in self.probes]
        inventory = eventually(lambda: self.client.tool("routing_windows"),
            lambda v: all(any(w["pid"] == p and w["title"].startswith(PREFIX) for w in v["windows"]) for p in pids), 60)
        windows = [next(w for w in inventory["windows"] if w["pid"] == p and w["title"].startswith(PREFIX)) for p in pids]
        for index, window in enumerate(windows):
            assert user32.SetWindowPos(window["hwnd"], wintypes.HWND(-1),
                30 + index % 2 * 460, 40 + index // 2 * 410, 420, 360, 0x10)
        inventory = self.client.tool("routing_windows")
        windows = [next(w for w in inventory["windows"] if w["pid"] == p and w["title"].startswith(PREFIX)) for p in pids]
        self.points = [(w["client"]["left"] + 120, w["client"]["top"] + 100) for w in windows]
        self.client.tool("routing_start", {"users": getattr(self, "user_names", ["A64", "B32", "C64", "D32"][:self.fixture_count]),
            "allowed_pids": pids, "mode": "concurrent", "multi_keyboard": True})
        self.before_selection()
        for index, (x, y) in enumerate(self.points):
            self.input("move_to", user=index, x=x, y=y)
            self.input("button", user=index, button="primary", pressed=True)
            self.input("button", user=index, button="primary", pressed=False)
        eventually(self.state, lambda s: [u["pid"] for u in s["users"]] == pids)
        eventually(self.reports, lambda rs: all(r["frames"] > 1 for r in rs))
        self.snapshot("selected-owned-x64-x86-edit-windows")

    def keyboard(self):
        representative = [("b", 66), ("7", 55), ("semicolon", 186),
                          ("leftshift", 160), ("rightctrl", 163), ("left", 37),
                          ("f5", 116), ("pause", 19), ("rightalt", 165), ("divide", 111)]
        for user in (0, 1):
            for key, vk in representative:
                baseline = self.report(user)["key_event_count"]
                other = self.report(1 - user)["key_event_count"]
                self.key(user, key, True)
                held = eventually(lambda: self.report(user), lambda r: vk in r["held_keys"] and r["key_event_count"] >= baseline + 1)
                self.key(user, key, False)
                released = eventually(lambda: self.report(user), lambda r: vk not in r["held_keys"] and r["key_event_count"] >= baseline + 2)
                events = released["key_events"][-2:]
                assert [(e["vk"], e["pressed"]) for e in events] == [(vk, True), (vk, False)], (key, events)
                assert all(e["scan"] > 0 for e in events), (key, events)
                if key in ("rightctrl", "left", "rightalt", "divide"):
                    assert all(e["extended"] for e in events), (key, events)
                assert self.report(1 - user)["key_event_count"] == other, "key leaked to other target"
                self.evidence["checks"][f"key-{user}-{key}"] = {"held": held, "released": released}
        self.clear_all()
        expected = ["A café 東京 🙂", "B Ελληνικά 🦊", "", ""]
        for user in (0, 1):
            self.input("text", user=user, text=expected[user])
        self.expect_texts(expected)
        before = self.reports()
        for args in ({"action": "key", "user": 0, "key": "f25", "pressed": True},
                     {"action": "text", "user": 0, "text": "bad\u0000text"},
                     {"action": "text", "user": 0, "text": "x" * 4097},
                     {"action": "text", "user": 99, "text": "out-of-scope"}):
            self.client.tool("routing_input", args, error=True)
        assert self.texts() == expected
        assert [r["key_event_count"] for r in self.reports()] == [r["key_event_count"] for r in before]
        self.snapshot("unicode-exact-edit-readback-and-validation")

    def macros(self):
        self.clear_all()
        self.client.tool("automation_record", {"recording": True})
        self.input("motion", user=0, dx=5, dy=3)
        self.input("button", user=0, button="primary", pressed=True)
        self.input("button", user=0, button="primary", pressed=False)
        self.tap(0, "f5")
        self.input("text", user=0, text="宏🙂")
        stopped = self.client.tool("automation_record", {"recording": False})
        clip = stopped["automation"]["clip"]
        assert len(clip["events"]) == 6, clip
        assert all(e["action"]["user"] == 0 for e in clip["events"])
        assert [e["at_micros"] for e in clip["events"]] == sorted(e["at_micros"] for e in clip["events"])
        saved = self.run / "recorded-macro.json"
        saved.write_text(json.dumps(clip, ensure_ascii=False), encoding="utf-8")
        loaded = self.client.tool("automation_clip", {"action": "load", "clip": json.loads(saved.read_text(encoding="utf-8"))})
        assert loaded["automation"]["clip"] == clip
        assert self.client.tool("automation_clip", {"action": "inspect"})["automation"]["clip"] == clip
        self.clear_text(0)
        before = self.report(0)
        self.client.tool("automation_play", {"loops": 2, "speed": 2.0})
        eventually(self.state, lambda s: s["automation"]["playback"]["state"] == "completed")
        self.expect_texts(["宏🙂宏🙂", "", "", ""])
        after = self.report(0)
        assert after["key_event_count"] == before["key_event_count"] + 4
        assert [(e["vk"], e["pressed"]) for e in after["key_events"][-4:]] == [(116, True), (116, False)] * 2
        self.released()
        self.snapshot("record-save-load-two-loop-native-playback")
        self.client.tool("automation_play", {"loops": 0, "speed": 1.0}, error=True)
        self.client.tool("automation_play", {"loops": 1, "speed": 101.0}, error=True)
        bad = {"version": 1, "events": [{"at_micros": 86_400_000_001, "action": {"type": "text", "user": 0, "text": "late-invalid"}}]}
        self.client.tool("automation_clip", {"action": "load", "clip": bad}, error=True)
        assert self.client.tool("automation_clip", {"action": "inspect"})["automation"]["clip"] == clip
        self.client.tool("automation_record", {"recording": True})
        cancelled = self.client.tool("automation_cancel")
        assert not cancelled["automation"]["recording"], "cancel left recording active"
        self.snapshot("invalid-macro-preserves-clip-and-record-cancel")

    def replication(self):
        self.clear_all()
        def graph(edges):
            return self.client.tool("routing_replication", {"edges": edges})
        def edge(a, b, **filters):
            return {"source": a, "target": b, **filters}
        graph([edge(0, 1), edge(0, 2)])
        baseline = self.reports()
        self.input("text", user=0, text="F🙂")
        self.tap(0, "f5")
        self.input("motion", user=0, dx=7, dy=4)
        self.expect_texts(["F🙂", "F🙂", "F🙂", ""])
        eventually(self.reports, lambda rs: all(rs[i]["key_event_count"] == baseline[i]["key_event_count"] + 2 for i in (0, 1, 2)))
        eventually(self.reports, lambda rs: all(rs[i]["cursor"] == [baseline[i]["cursor"][0] + 7, baseline[i]["cursor"][1] + 4] for i in (0, 1, 2)))
        after = self.reports()
        assert after[3]["key_event_count"] == baseline[3]["key_event_count"]
        for i in (0, 1, 2):
            assert after[i]["cursor"] == [baseline[i]["cursor"][0] + 7, baseline[i]["cursor"][1] + 4], (i, baseline[i], after[i])
        assert after[3]["cursor"] == baseline[3]["cursor"]
        self.snapshot("isolated-fanout-native-key-text-motion")
        graph([edge(0, 1), edge(1, 2)])
        self.input("text", user=0, text="C")
        self.expect_texts(["F🙂C", "F🙂C", "F🙂C", ""])
        accepted = self.state()["automation"]["edges"]
        for invalid in ([edge(0, 1), edge(1, 0)], [edge(0, 0)], [edge(0, 99)], [edge(0, 1)] * 4097):
            self.client.tool("routing_replication", {"edges": invalid}, error=True)
            assert self.state()["automation"]["edges"] == accepted, "rejected graph changed live graph"
        self.input("text", user=0, text="N")
        self.expect_texts(["F🙂CN", "F🙂CN", "F🙂CN", ""])
        self.snapshot("chain-cycle-self-boundary-rejection-no-amplification")
        graph([edge(0, 1), edge(0, 2), edge(1, 3), edge(2, 3)])
        baseline = self.reports()
        self.input("text", user=0, text="D")
        self.tap(0, "f5")
        self.expect_texts(["F🙂CND", "F🙂CND", "F🙂CND", "D"])
        eventually(self.reports, lambda rs: all(rs[i]["key_event_count"] == baseline[i]["key_event_count"] + 2 for i in range(4)))
        for r in self.reports():
            assert [(e["vk"], e["pressed"]) for e in r["key_events"][-2:]] == [(116, True), (116, False)]
        self.snapshot("diamond-once-per-target-native-ordered-key-traces")
        graph([edge(0, 1, mouse=False, keyboard=False, text=True)])
        baseline = self.reports()
        self.input("text", user=0, text="T")
        self.tap(0, "f5")
        self.expect_texts(["F🙂CNDT", "F🙂CNDT", "F🙂CND", "D"])
        eventually(lambda: self.report(0), lambda r: r["key_event_count"] == baseline[0]["key_event_count"] + 2)
        assert self.report(1)["key_event_count"] == baseline[1]["key_event_count"]
        self.snapshot("category-filter-native-delivery")
        graph([])

    def cleanup_playback(self):
        keyboard = 0xF1000003
        mouse = 0xF1000002
        clip = {"version": 1, "events": [
            {"at_micros": 0, "action": {"type": "packet", "user": 0, "packet": {"device": keyboard, "kind": 1, "flags": 0, "x": 63, "y": 0, "data": 116}}},
            {"at_micros": 0, "action": {"type": "packet", "user": 0, "packet": {"device": mouse, "kind": 0, "flags": 1, "x": 0, "y": 0, "data": 0}}},
            {"at_micros": 2_000_000, "action": {"type": "text", "user": 0, "text": "late-playback-must-not-arrive"}}]}
        self.client.tool("automation_clip", {"action": "load", "clip": clip})
        self.client.tool("automation_play", {"loops": None, "speed": 1.0})
        eventually(lambda: self.report(0), lambda r: 116 in r["held_keys"] and r["buttons"] == 1)
        self.snapshot("before-cancel-held-native-input")
        self.client.tool("automation_cancel")
        self.released()
        text = self.texts()
        time.sleep(2.2)
        assert self.texts() == text, "cancelled playback emitted late text"
        assert self.state()["automation"]["playback"]["state"] == "cancelled"
        self.snapshot("cancel-releases-and-prevents-late-playback")
        self.client.tool("automation_play", {"loops": None, "speed": 1.0})
        eventually(lambda: self.report(0), lambda r: 116 in r["held_keys"] and r["buttons"] == 1)
        before = self.state()
        overlays = [u["overlay"]["hwnd"] for u in before["users"]]
        stopped = self.client.tool("routing_stop")
        assert not stopped["running"] and not stopped["users"]
        self.released()
        eventually(lambda: [bool(user32.IsWindow(h)) for h in overlays], lambda live: not any(live))
        text = self.texts()
        time.sleep(2.2)
        assert self.texts() == text, "stopped playback emitted late text"
        self.snapshot("routing-stop-releases-destroys-overlays-no-late-playback")
        assert self.client.tool("routing_stop") == self.state()

    def close(self):
        if self.client:
            try:
                self.client.close()
            except Exception:
                self.client.process.kill()
                self.client.process.wait()
        for process in self.probes:
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=10)
        self.evidence["owned_processes_closed"] = all(p.poll() is not None for p in self.probes)
        self.save()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    args = parser.parse_args()
    gate = Gate(args)
    try:
        gate.start()
        gate.keyboard()
        gate.macros()
        gate.replication()
        gate.cleanup_playback()
        gate.evidence["result"] = "PASS"
        print(f"PASS: independent native automation acceptance. Evidence: {gate.run}", flush=True)
    except Exception as error:
        gate.evidence["result"] = "NOT ACCEPTED"
        gate.evidence["failure"] = repr(error)
        print(f"NOT ACCEPTED: {error!a}. Evidence: {gate.run}", flush=True)
        raise
    finally:
        gate.close()


if __name__ == "__main__":
    main()
