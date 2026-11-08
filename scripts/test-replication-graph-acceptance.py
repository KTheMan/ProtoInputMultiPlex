"""Independent finite stable-ID replication gate. Run only after producer READY.

Four disposable virtual x64/x86 targets; no hardware/global input. A second fresh
fixture set restores a saved definition with reordered IDs and renamed users.
UI identity migration/history/canvas/profile checks are a separate finite gate.
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

_spec = importlib.util.spec_from_file_location("graph_studio_helpers", Path(__file__).with_name("test-macro-studio-acceptance.py"))
_studio = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_studio)
Base, Client, eventually, user32 = _studio.Gate, _studio._base.Client, _studio.eventually, _studio.user32


def edge(source, target, **changes):
    result = {"source": source, "target": target, "mouse": True, "keyboard": True, "text": True}
    result.update(changes)
    return result


def definition(edges):
    return {"version": 1, "edges": edges}


class GraphGate(Base):
    run_prefix = "acceptance-replication-graph"
    tool_names = _studio.StudioGate.tool_names | {"routing_graph"}

    def __init__(self, args, restored=None):
        super().__init__(args)
        self.restored = restored
        self.order = [3, 1, 0, 2] if restored else list(range(4))

    def report(self, user):
        return super().report(self.order[user])

    def start(self):
        # Keep physical fixture ownership/architecture independent of dense seat order.
        images = [self.run / "control64.exe", self.run / "probe32.exe"]
        for source, target in zip((self.args.executable, self.args.x86_executable), images):
            shutil.copy2(source, target)
        for image, machine in zip(images, (0x8664, 0x14C)):
            with image.open("rb") as stream:
                assert stream.read(2) == b"MZ"
                stream.seek(0x3C)
                stream.seek(struct.unpack("<I", stream.read(4))[0])
                assert stream.read(4) == b"PE\0\0" and struct.unpack("<H", stream.read(2))[0] == machine
        self.evidence["physical_fixture_architectures"] = ["x64", "x86", "x64", "x86"]
        self.evidence["architectures"] = [self.evidence["physical_fixture_architectures"][i] for i in self.order]
        self.evidence["build_sha256"] = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in images}
        for i in range(4):
            self.probes.append(subprocess.Popen([str(images[i % 2]), "probe", "--automation", "--report", str(self.run / f"probe-{i}.json")], creationflags=subprocess.CREATE_NO_WINDOW))
        self.client = Client(images[0], self.args.runtime.resolve(), self.run / "mcp.stderr.log")
        tools = self.client.rpc("tools/list", {})["tools"]
        assert {t["name"] for t in tools} == self.tool_names and len(tools) == 17
        assert all(t["inputSchema"]["type"] == "object" for t in tools)
        schema = next(t["inputSchema"] for t in tools if t["name"] == "routing_graph")
        for required in ("expected_revision", "version", "source", "target", "mouse", "keyboard", "text"):
            assert required in json.dumps(schema), required
        self.evidence["tools"] = tools
        pids = [p.pid for p in self.probes]
        inventory = eventually(lambda: self.client.tool("routing_windows"), lambda v: all(any(w["pid"] == p and w["title"].startswith(_studio._base.PREFIX) for w in v["windows"]) for p in pids), 60)
        for i, pid in enumerate(pids):
            window = next(w for w in inventory["windows"] if w["pid"] == pid and w["title"].startswith(_studio._base.PREFIX))
            assert user32.SetWindowPos(window["hwnd"], wintypes.HWND(-1), 30 + i % 2 * 460, 40 + i // 2 * 410, 420, 360, 0x10)
        inventory = self.client.tool("routing_windows")
        windows = [next(w for w in inventory["windows"] if w["pid"] == p and w["title"].startswith(_studio._base.PREFIX)) for p in pids]
        physical_points = [[w["client"]["left"] + 120, w["client"]["top"] + 100] for w in windows]
        self.points = [physical_points[i] for i in self.order]
        names = ["Graph A64", "Graph B32", "Graph C64", "Graph D32"]
        args = {"users": [names[i] for i in self.order], "allowed_pids": pids, "mode": "concurrent", "multi_keyboard": True}
        if self.restored:
            args.update(users=["Renamed " + names[i] for i in self.order], user_ids=[self.restored["ids"][i] for i in self.order], graph=self.restored["definition"])
        self.client.tool("routing_start", args)
        initialization = [self.order.index(i) for i in range(4)] if self.restored else list(range(4))
        for user in initialization:
            x, y = self.points[user]
            self.input("move_to", user=user, x=x, y=y)
            self.input("button", user=user, button="primary", pressed=True)
            self.input("button", user=user, button="primary", pressed=False)
        expected = [pids[i] for i in self.order]
        eventually(self.state, lambda s: [u["pid"] for u in s["users"]] == expected)
        eventually(self.reports, lambda rs: all(r["cursor"] == self.points[i] and not r["buttons"] and not r["held_keys"] for i, r in enumerate(rs)))
        eventually(self.state, lambda s: all([u["overlay"]["x"], u["overlay"]["y"]] == self.points[i] for i, u in enumerate(s["users"])))
        identities = self.client.tool("routing_graph", {"operation": "inspect"})["identities"]
        self.ids = [u["id"] for u in identities]
        assert [u["user"] for u in identities] == list(range(4)) and len(set(self.ids)) == 4
        if self.restored:
            assert self.ids == [self.restored["ids"][i] for i in self.order]
            assert self.graph()["definition"] == self.restored["definition"]
        self.snapshot("seventeen-object-schemas-owned-fixtures-stable-identities")

    def graph(self):
        return self.client.tool("routing_graph", {"operation": "inspect"})["graph"]

    def apply(self, value, error=False, revision=None):
        if revision is None:
            revision = self.graph()["revision"]
        return self.client.tool("routing_graph", {"operation": "apply", "graph": value, "expected_revision": revision}, error=error)

    def unchanged(self, before, held=True):
        assert self.graph() == before, "rejected candidate mutated graph/revision"
        if held:
            eventually(self.reports, lambda rs: all(116 in r["held_keys"] and r["buttons"] & 8 for r in rs))

    def atomicity(self):
        a, b, c, d = self.ids
        diamond = definition([edge(a, b), edge(a, c), edge(b, d), edge(c, d)])
        prior = self.graph()["revision"]
        self.apply(diamond)
        assert self.graph() == {"revision": prior + 1, "definition": diamond}
        self.key(0, "f5", True)
        self.input("button", user=0, button="back", pressed=True)
        self.unchanged(self.graph())
        before = self.graph()
        invalid = [definition([edge(a, a)]), definition([edge(a, b), edge(b, a)]), definition([edge(a, b), edge(a, b)]),
                   definition([edge(a, b, mouse=False, keyboard=False, text=False)]), definition([edge(a, "missing-seat")]), {"version": 2, "edges": []}]
        for value in invalid:
            self.apply(value, error=True)
            self.unchanged(before)
        self.apply(definition([]), error=True, revision=before["revision"] - 1)
        self.unchanged(before)
        # Numeric compatibility converts endpoints to the very same ID definition.
        self.key(0, "f5", False)
        self.input("button", user=0, button="back", pressed=False)
        self.released()
        numeric = [{**e, "source": self.ids.index(e["source"]), "target": self.ids.index(e["target"])} for e in diamond["edges"]]
        self.client.tool("routing_replication", {"edges": numeric})
        assert self.graph()["definition"] == diamond and self.graph()["revision"] == before["revision"] + 1
        self.evidence["saved_definition"] = diamond
        path = self.run / "saved-id-graph.json"
        path.write_text(json.dumps({"ids": self.ids, "definition": diamond}), encoding="utf-8")
        self.snapshot("stable-typed-and-numeric-graph-atomic-rejections-preserve-native-holds")
        return json.loads(path.read_text(encoding="utf-8"))

    def automation_boundary(self):
        before = self.graph()
        holding = _studio.clip(_studio.event(0, _studio.key(0, True)), _studio.event(0, _studio.button(0, True)), _studio.event(20_000_000, _studio.key(0, False)))
        self.client.tool("macro_studio", {"operation": "load", "user": 0, "clip": holding})
        self.client.tool("macro_studio", {"operation": "play", "user": 0, "options": {}})
        eventually(self.reports, lambda rs: all(116 in r["held_keys"] and r["buttons"] & 1 for r in rs))
        self.apply(definition([]), error=True)
        self.client.tool("routing_replication", {"edges": []}, error=True)
        assert self.graph() == before
        eventually(self.reports, lambda rs: all(116 in r["held_keys"] and r["buttons"] & 1 for r in rs))
        assert self.state()["macro_studio"]["users"][0]["playback"]["state"] == "playing"
        self.client.tool("automation_cancel")
        self.released()
        self.client.tool("macro_studio", {"operation": "record", "user": 0, "active": True})
        self.key(0, "f5", True)
        self.input("button", user=0, button="primary", pressed=True)
        self.apply(definition([]), error=True)
        self.client.tool("routing_replication", {"edges": []}, error=True)
        assert self.graph() == before and self.state()["macro_studio"]["users"][0]["recording"]
        eventually(self.reports, lambda rs: all(116 in r["held_keys"] and r["buttons"] & 1 for r in rs))
        self.client.tool("automation_cancel")
        self.released()
        for phase in ("record", "play"):
            if phase == "record":
                self.client.tool("automation_record", {"recording": True})
                self.key(0, "f5", True)
                self.input("button", user=0, button="primary", pressed=True)
            else:
                self.client.tool("automation_clip", {"action": "load", "clip": holding})
                self.client.tool("automation_play", {"loops": None, "speed": 1.})
            eventually(self.reports, lambda rs: all(116 in r["held_keys"] and r["buttons"] & 1 for r in rs))
            self.apply(definition([]), error=True)
            self.client.tool("routing_replication", {"edges": []}, error=True)
            assert self.graph() == before
            eventually(self.reports, lambda rs: all(116 in r["held_keys"] and r["buttons"] & 1 for r in rs))
            state = self.state()["automation"]
            assert state["recording"] if phase == "record" else state["playback"]["state"] == "playing"
            self.client.tool("automation_cancel")
            self.released()
        self.snapshot("studio-and-composite-active-reject-graph-before-native-release")

    def diamond_delivery(self):
        # Reordered source A is dense index 2; identities are deliberately renamed.
        source = self.ids.index(self.restored["ids"][0])
        starts = [r["native_event_count"] for r in self.reports()]
        cursors = [r["cursor"] for r in self.reports()]
        self.input("text", user=source, text="Diamond café 🙂")
        self.key(source, "f5", True)
        eventually(self.reports, lambda rs: all(116 in r["held_keys"] for r in rs))
        time.sleep(.15)
        self.key(source, "f5", False)
        self.input("button", user=source, button="back", pressed=True)
        eventually(self.reports, lambda rs: all(r["buttons"] & 8 for r in rs))
        self.input("button", user=source, button="back", pressed=False)
        self.input("motion", user=source, dx=7, dy=3)
        eventually(self.reports, lambda rs: all(r["text"] == "Diamond café 🙂" and r["cursor"] == [cursors[i][0] + 7, cursors[i][1] + 3] and not r["held_keys"] and not r["buttons"] for i, r in enumerate(rs)))
        for user in range(4):
            report = self.report(user)
            assert report["native_event_count"] == len(report["native_events"]), "native trace truncated"
            trace = report["native_events"][starts[user]:]
            meaningful = [(e["kind"], e["code"], e["pressed"]) for e in trace if e["kind"] != "motion"]
            expected = [("char", c, None) for c in _studio.utf16("Diamond café 🙂")] + [("key", 116, True), ("key", 116, False), ("button", 3, True), ("button", 3, False)]
            assert meaningful == expected, (user, trace)
            keys = [e for e in trace if e["kind"] == "key"]
            assert keys[1]["at_us"] - keys[0]["at_us"] >= 140_000, keys
        self.snapshot("fresh-renamed-reordered-stable-id-diamond-exact-native-unicode-key-mouse-motion-timing")
        self.client.tool("routing_stop")
        self.released()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--restore", type=Path)
    args = parser.parse_args()
    saved = json.loads(args.restore.read_text(encoding="utf-8")) if args.restore else None
    for phase in (("fresh-restoration",) if args.restore else ("mutations", "fresh-restoration")):
        gate = GraphGate(args, restored=saved)
        try:
            gate.start()
            if saved is None:
                saved = gate.atomicity()
                gate.automation_boundary()
                gate.client.tool("routing_stop")
            else:
                gate.diamond_delivery()
            gate.evidence["result"] = "PASS"
            gate.evidence["phase"] = phase
            print(f"PASS: finite graph {phase}. Evidence: {gate.run}", flush=True)
        except Exception as error:
            gate.evidence["result"] = "NOT ACCEPTED"
            gate.evidence["failure"] = repr(error)
            print(f"NOT ACCEPTED: {error!a}. Evidence: {gate.run}", flush=True)
            raise
        finally:
            gate.close()


if __name__ == "__main__":
    main()
