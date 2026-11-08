"""Independent bounded generated-seat gate. Launch only after producer READY.

Every PID, window and native event belongs to these disposable virtual fixtures.
No OS input injection or hardware/RustDesk control. Historical feature gates are
not repeated; removal exercises only the newly dynamic resource/route lifecycle.
"""
import argparse
import copy
from ctypes import wintypes
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import struct
import subprocess
import time
from PIL import Image

_spec = importlib.util.spec_from_file_location("virtual_studio_helpers", Path(__file__).with_name("test-macro-studio-acceptance.py"))
_studio = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_studio)
eventually, user32 = _studio.eventually, _studio.user32
user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]


class VirtualGate(_studio.StudioGate):
    fixture_count = 6
    run_prefix = "acceptance-virtual-users"
    tool_names = _studio.StudioGate.tool_names | {"virtual_users"}

    def start(self):
        images = [self.run / "control64.exe", self.run / "probe32.exe"]
        for source, target, machine in zip((self.args.executable, self.args.x86_executable), images, (0x8664, 0x14c)):
            shutil.copy2(source, target)
            with target.open("rb") as stream:
                assert stream.read(2) == b"MZ"
                stream.seek(0x3c)
                stream.seek(struct.unpack("<I", stream.read(4))[0])
                assert stream.read(4) == b"PE\0\0" and struct.unpack("<H", stream.read(2))[0] == machine
        self.evidence["build_sha256"] = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in images}
        self.evidence["architectures"] = ["x64", "x86"] * 3
        for i in range(self.fixture_count):
            self.probes.append(subprocess.Popen([str(images[i % 2]), "probe", "--automation", "--report", str(self.run / f"probe-{i}.json"), "--capture-pattern", str(0x286098 + i * 0x10101)], creationflags=subprocess.CREATE_NO_WINDOW))
        self.client = _studio._base.Client(images[0], self.args.runtime.resolve(), self.run / "mcp.stderr.log")
        tools = self.client.rpc("tools/list", {})["tools"]
        assert {t["name"] for t in tools} == self.tool_names and len(tools) == 18
        assert all(t["inputSchema"]["type"] == "object" for t in tools)
        schema = json.dumps(next(t["inputSchema"] for t in tools if t["name"] == "virtual_users"))
        for name in ("inspect", "start", "stop", "source_id", "target_pid", "expected_revision", "options"):
            assert name in schema, name
        self.evidence["tools"] = tools
        pids = [p.pid for p in self.probes]
        inventory = eventually(lambda: self.client.tool("routing_windows"), lambda v: all(any(w["pid"] == p and w["title"].startswith(_studio._base.PREFIX) for w in v["windows"]) for p in pids), 60)
        for i, pid in enumerate(pids):
            window = next(w for w in inventory["windows"] if w["pid"] == pid and w["title"].startswith(_studio._base.PREFIX))
            assert user32.SetWindowPos(window["hwnd"], wintypes.HWND(-1), 30 + i % 3 * 460, 40 + i // 3 * 410, 420, 360, 0x10)
        inventory = self.client.tool("routing_windows")
        windows = [next(w for w in inventory["windows"] if w["pid"] == p and w["title"].startswith(_studio._base.PREFIX)) for p in pids]
        self.points = [[w["client"]["left"] + 120, w["client"]["top"] + 100] for w in windows]
        self.centers = [[(w["client"]["left"] + w["client"]["right"]) // 2, (w["client"]["top"] + w["client"]["bottom"]) // 2] for w in windows]
        self.original_edge = {"source": "virtual-original-c", "target": "virtual-original-a", "mouse": False, "keyboard": False, "text": True}
        self.client.tool("routing_start", {"users": ["Original A64", "Original C64"], "user_ids": ["virtual-original-a", "virtual-original-c"], "allowed_pids": [pids[i] for i in (0, 1, 2, 3, 5)], "mode": "concurrent", "multi_keyboard": True, "graph": {"version": 1, "edges": [self.original_edge]}})
        for user, physical in enumerate((0, 2)):
            self.input("move_to", user=user, x=self.points[physical][0], y=self.points[physical][1])
            self.input("button", user=user, button="primary", pressed=True)
            self.input("button", user=user, button="primary", pressed=False)
        eventually(self.state, lambda s: [u["pid"] for u in s["users"]] == [pids[0], pids[2]])
        eventually(self.reports, lambda rs: all(r["frames"] > 1 for r in rs) and rs[0]["cursor"] == self.points[0] and rs[2]["cursor"] == self.points[2])
        self.ids = [s["seat"]["id"] for s in self.roster()["slots"]]
        assert user32.PostMessageW(windows[5]["hwnd"], 0x10, 0, 0), "close only owned exited fixture"
        self.probes[5].wait(timeout=10)
        self.snapshot("eighteen-object-schemas-two-originals-owned-native-fixtures")

    def virtual(self, operation="inspect", error=False, **fields):
        return self.client.tool("virtual_users", {"operation": operation, **fields}, error=error)

    def roster(self):
        return self.virtual()["virtual_users"]

    def generated(self, source, physical, name, options=None):
        before = self.roster()
        state = self.virtual("start", source_id=source, target_pid=self.probes[physical].pid, name=name, options=options or {}, expected_revision=before["revision"])["virtual_users"]
        assert state["error"] is None and state["revision"] == before["revision"] + 1
        assert len(state["slots"]) == len(before["slots"]) + 1
        slot = state["slots"][-1]
        assert slot["active"] and slot["clone_of"] == source and slot["user"] == len(before["slots"])
        return slot

    def stopped(self, slot):
        before = self.roster()
        state = self.virtual("stop", id=slot["seat"]["id"], expected_revision=before["revision"])["virtual_users"]
        assert state["revision"] == before["revision"] + 1 and len(state["slots"]) == len(before["slots"])
        assert not state["slots"][slot["user"]]["active"]

    def originals_held(self):
        eventually(self.reports, lambda rs: 116 in rs[0]["held_keys"] and rs[0]["buttons"] & 8 and 117 in rs[2]["held_keys"])
        assert self.active(0), "clone lifecycle cancelled source Studio"

    def source_program(self, events):
        self.load(0, _studio.clip(*events))
        self.play(0)

    def attach_and_atomicity(self):
        self.client.tool("routing_settings", {"user": 0, "settings": {"speed_x": 1.25, "speed_y": 1.5, "badge": "source"}})
        settings = self.roster()["slots"][0]["seat"]["cursor"]
        self.key(1, "f6", True)
        self.source_program([_studio.event(0, _studio.key(0, True)), _studio.event(0, _studio.button(0, True, 3)), _studio.event(120_000_000, _studio.key(0, False))])
        self.originals_held()
        before = self.reports()
        self.clone = self.generated(self.ids[0], 1, "Generated B32")
        clone = self.clone
        assert clone["seat"]["cursor"] == settings
        assert clone["seat"]["annotation"]["tool"] == "off" and clone["seat"]["magnifier"]["mode"] == "off"
        assert self.lane(clone["user"])["loaded_events"] == 0 and self.lane(clone["user"])["playback"] is None
        eventually(lambda: self.report(1), lambda r: r["cursor"] == self.centers[1])
        rs = self.reports()
        assert rs[1]["mouse_event_count"] == before[1]["mouse_event_count"] and rs[1]["key_event_count"] == before[1]["key_event_count"]
        self.originals_held()
        graph = self.client.tool("routing_graph", {"operation": "inspect"})["graph"]
        roster = self.roster()
        variants = [dict(source_id="missing", target_pid=self.probes[3].pid, name="Missing"), dict(source_id=self.ids[0], target_pid=self.probes[4].pid, name="Outside"), dict(source_id=self.ids[0], target_pid=self.probes[5].pid, name="Exited"), dict(source_id=self.ids[0], target_pid=self.probes[1].pid, name="Duplicate target"), dict(source_id=self.ids[0], target_pid=self.probes[3].pid, name="Stale", expected_revision=0), dict(source_id=self.ids[0], target_pid=self.probes[3].pid, name="Bad option", options={"step_interval_ms": 1001})]
        for fields in variants:
            self.virtual("start", error=True, **fields)
            current = self.roster()
            assert current["revision"] == roster["revision"] and current["slots"] == roster["slots"]
            assert self.client.tool("routing_graph", {"operation": "inspect"})["graph"] == graph
            self.originals_held()
        self.virtual("stop", id=self.ids[0], error=True)
        self.virtual("stop", id=clone["seat"]["id"], expected_revision=0, error=True)
        self.originals_held()
        self.snapshot("click-free-center-attachment-copied-settings-empty-lanes-invalid-cas-preserve-originals")

    def delivery(self):
        # A new source program produces future events through the newly added edge.
        # It is never interrupted by manual source input while playback is active.
        self.cancel(0)
        before = self.reports()
        start = before[1]["native_event_count"]
        self.source_program([_studio.event(0, _studio.key(0, True)), _studio.event(0, _studio.button(0, True, 3)), _studio.event(150_000, _studio.text(0, "Clone café 🙂")), _studio.event(250_000, _studio.move(0, [self.points[0][0] + 6, self.points[0][1] + 3])), _studio.event(120_000_000, _studio.key(0, False))])
        eventually(self.reports, lambda rs: all(rs[i]["text"] == before[i]["text"] + "Clone café 🙂" and 116 in rs[i]["held_keys"] and rs[i]["buttons"] & 8 for i in (0, 1)) and rs[1]["cursor"] == [self.centers[1][0] + 6, self.centers[1][1] + 3])
        trace = self.report(1)["native_events"][start:]
        meaningful = [(e["kind"], e["code"], e["pressed"]) for e in trace if e["kind"] != "motion"]
        assert meaningful == [("key", 116, True), ("button", 3, True)] + [("char", c, None) for c in _studio.utf16("Clone café 🙂")], meaningful
        assert self.report(2)["text"] == before[2]["text"] and self.report(2)["key_event_count"] == before[2]["key_event_count"]
        self.originals_held()
        self.snapshot("actual-x86-child-key-unicode-once-and-independent-native-cursor-from-studio")

    def clone_resources_and_chain(self):
        self.cancel(0)
        self.child = self.generated(self.clone["seat"]["id"], 3, "Generated D32", {"click_delay_ms": 3000})
        self.key(self.child["user"], "f8", True)
        self.client.tool("annotation", {"operation": "configure", "user": self.clone["user"], "settings": {"tool": "brush", "activator": "middle", "color": [19, 43, 91], "width": 9}})
        self.input("button", user=self.clone["user"], button="middle", pressed=True)
        self.input("motion", user=self.clone["user"], dx=8, dy=0)
        self.input("button", user=self.clone["user"], button="middle", pressed=False)
        eventually(self.state, lambda s: s["annotation"]["users"][self.clone["user"]]["segments"] > 0)
        ink_before = self.run / "clone-ink-before-removal.png"
        self.client.tool("annotation", {"operation": "export", "user": self.clone["user"], "kind": "ink_only", "path": str(ink_before)})
        with Image.open(ink_before) as ink:
            assert ink.mode == "RGBA" and ink.getchannel("A").getbbox(), "clone stroke has no actual RGBA ink"
        self.client.tool("magnifier", {"operation": "configure", "user": self.clone["user"], "settings": {"mode": "lens", "auto_refresh": False, "width": 96, "height": 64}})
        mag = eventually(self.state, lambda s: s["magnifier"]["users"][self.clone["user"]]["overlay"] and s["magnifier"]["users"][self.clone["user"]]["overlay"]["visible"])
        self.clone_mag_hwnd = mag["magnifier"]["users"][self.clone["user"]]["overlay"]["hwnd"]
        self.clone_cursor_hwnd = next(u["overlay"]["hwnd"] for u in mag["users"] if u["name"] == self.clone["seat"]["name"])
        # Failed native registration attempts reserve device numbers; dense user
        # indexes must never be used to reconstruct generated packet device IDs.
        clone_device = int(self.clone["seat"]["keyboard"].rsplit("/", 1)[1])
        def clone_key(down):
            action = _studio.key(self.clone["user"], down, 118, 65)
            action["packet"]["device"] = 0xF1000001 + clone_device * 2
            return action
        self.load(self.clone["user"], _studio.clip(_studio.event(0, clone_key(True)), _studio.event(120_000_000, clone_key(False))))
        self.play(self.clone["user"])
        self.source_program([_studio.event(0, _studio.key(0, True)), _studio.event(0, _studio.button(0, True, 3)), _studio.event(120_000_000, _studio.key(0, False))])
        eventually(self.reports, lambda rs: all(116 in rs[i]["held_keys"] for i in (0, 1, 3)) and 118 in rs[1]["held_keys"] and 119 in rs[3]["held_keys"])
        eventually(self.state, lambda s: s["automation"]["replication"]["pending_events"] > 0)
        # The delayed A->clone->child button copy is still queued. Removal must
        # release downstream copied keys while preserving child's manual F8.
        self.stopped(self.clone)
        eventually(self.reports, lambda rs: not rs[1]["held_keys"] and not rs[1]["buttons"] and rs[3]["held_keys"] == [119] and not rs[3]["buttons"])
        self.originals_held()
        after = eventually(self.state, lambda s: s["annotation"]["users"][self.clone["user"]]["segments"] == 0 and s["macro_studio"]["users"][self.clone["user"]]["loaded_events"] == 0)
        assert after["automation"]["replication"]["pending_events"] == 0
        edges = self.client.tool("routing_graph", {"operation": "inspect"})["graph"]["definition"]["edges"]
        assert all(self.clone["seat"]["id"] not in (e["source"], e["target"]) for e in edges)
        assert [{k: e[k] for k in self.original_edge} for e in edges] == [self.original_edge], "clone removal changed the original graph"
        assert after["annotation"]["users"][self.clone["user"]]["segments"] == 0
        assert after["macro_studio"]["users"][self.clone["user"]]["loaded_events"] == 0
        ink_after = self.run / "clone-ink-after-removal.png"
        self.client.tool("annotation", {"operation": "export", "user": 0, "kind": "ink_only", "path": str(ink_after)})
        with Image.open(ink_after) as ink:
            assert ink.mode == "RGBA" and ink.getchannel("A").getbbox() is None, "retired clone ink survived"
        eventually(lambda: [bool(user32.IsWindow(h)) for h in (self.clone_mag_hwnd, self.clone_cursor_hwnd)], lambda xs: not any(xs))
        prior = self.report(3)["mouse_event_count"]
        time.sleep(3.2)
        assert self.report(3)["mouse_event_count"] == prior, "removed crossing route delivered late button"
        for fields in ({"action": "text", "user": self.clone["user"], "text": "retired"}, {"action": "key", "user": self.clone["user"], "key": "f5", "pressed": True}):
            self.client.tool("routing_input", fields, error=True)
        self.virtual("stop", id=self.clone["seat"]["id"], error=True)
        self.virtual("start", source_id=self.clone["seat"]["id"], target_pid=self.probes[1].pid, name="Retired source", error=True)
        self.originals_held()
        self.snapshot("intermediate-clone-removal-discards-crossing-tail-resources-and-replica-holds-only")

    def replacement_and_eof(self):
        prior = self.roster()
        replacement = self.generated(self.ids[0], 1, "Generated replacement B32")
        assert replacement["user"] == len(prior["slots"]) and replacement["user"] > self.child["user"]
        ids = [s["seat"]["id"] for s in self.roster()["slots"]]
        devices = [p for s in self.roster()["slots"] for p in (s["seat"]["mouse"], s["seat"]["keyboard"])]
        assert len(ids) == len(set(ids)) and len(devices) == len(set(devices))
        self.key(replacement["user"], "f9", True)
        eventually(lambda: self.report(1), lambda r: 120 in r["held_keys"])
        self.originals_held()
        before = self.state()
        hwnds = [u["overlay"]["hwnd"] for u in before["users"] if u["overlay"]]
        self.evidence["before_eof"] = before
        self.client.close()
        self.evidence["eof_controller_exit_code"] = self.client.process.poll()
        assert self.evidence["eof_controller_exit_code"] == 0
        self.client = None
        self.released()
        eventually(lambda: [bool(user32.IsWindow(h)) for h in hwnds], lambda xs: not any(xs))
        self.evidence["checks"]["nonrecycled-index-id-devices-reused-owned-target-and-eof-global-cleanup"] = {"roster_before": prior, "replacement": replacement, "before_eof": before, "probes_after": self.reports(), "destroyed_hwnds": hwnds}
        self.save()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--ready", action="store_true", help="Producer declared immutable builds and exclusive live slot")
    args = parser.parse_args()
    assert args.ready, "No live acceptance before producer READY"
    gate = VirtualGate(args)
    try:
        gate.start()
        gate.attach_and_atomicity()
        gate.delivery()
        gate.clone_resources_and_chain()
        gate.replacement_and_eof()
        gate.evidence["result"] = "PASS"
        print(f"PASS: finite virtual-user native gate. Evidence: {gate.run}", flush=True)
    except Exception as error:
        gate.evidence["result"] = "NOT ACCEPTED"
        gate.evidence["failure"] = repr(error)
        print(f"NOT ACCEPTED: {error!a}. Evidence: {gate.run}", flush=True)
        raise
    finally:
        gate.close()


if __name__ == "__main__":
    main()
