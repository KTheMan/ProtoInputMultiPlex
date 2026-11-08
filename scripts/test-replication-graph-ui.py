"""Finite real Ouroboros graph/history/identity gate, including disabled controls.

Offline runs use an owned working directory containing no runtime DLLs. Live
variants own every target, preserve stable seat IDs across fresh profile restart,
and decode actual delivered native messages. No OS/hardware input is synthesized.
"""
import argparse
import copy
from ctypes import wintypes
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import time
from PIL import Image

_spec = importlib.util.spec_from_file_location("graph_native_helpers", Path(__file__).with_name("test-replication-graph-acceptance.py"))
_native = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_native)
Client, eventually, user32 = _native.Client, _native.eventually, _native.user32


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def put(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False), encoding="utf-8")


def step(name, button, fields=None, point=None, user=0, disabled=False):
    result = {"id": name, "button": button, "wait_ms": 550, "wait_for_sample": True,
              "fields": {k: str(v) for k, v in (fields or {}).items()}}
    if point is not None:
        result.update(point=point, user=user)
    if disabled:
        result["expect_disabled"] = True
    return result


class UiGate:
    def __init__(self, args, label, offline=False, count=0):
        self.args, self.offline, self.count = args, offline, count
        self.run = args.runtime.resolve() / time.strftime("acceptance-graph-" + label + "-%Y%m%d-%H%M%S")
        self.run.mkdir(parents=True)
        self.images = [self.run / "control64.exe", self.run / "probe32.exe"]
        for source, image in zip((args.executable, args.x86_executable), self.images):
            shutil.copy2(source, image)
        self.owned = []
        self.evidence = {"result": "NOT ACCEPTED", "phase": label, "samples": {},
                         "build_sha256": {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in self.images}}
        self.pids, self.points = [], []

    def save(self):
        put(self.run / "evidence.json", self.evidence)

    def fixtures(self):
        for i in range(self.count):
            self.owned.append(subprocess.Popen([str(self.images[i % 2]), "probe", "--automation", "--report", str(self.run / f"probe-{i}.json")], creationflags=subprocess.CREATE_NO_WINDOW))
        self.pids = [p.pid for p in self.owned]
        client = Client(self.images[0], self.args.runtime.resolve(), self.run / "discovery.stderr.log")
        try:
            tools = client.rpc("tools/list", {})["tools"]
            assert {t["name"] for t in tools} == _native.GraphGate.tool_names and len(tools) == 17
            inventory = eventually(lambda: client.tool("routing_windows"), lambda v: all(any(w["pid"] == p and w["title"].startswith(_native._studio._base.PREFIX) for w in v["windows"]) for p in self.pids), 60)
            for i, pid in enumerate(self.pids):
                w = next(w for w in inventory["windows"] if w["pid"] == pid and w["title"].startswith(_native._studio._base.PREFIX))
                assert user32.SetWindowPos(w["hwnd"], wintypes.HWND(-1), 30 + i % 2 * 460, 40 + i // 2 * 410, 420, 360, 0x10)
            inventory = client.tool("routing_windows")
            windows = [next(w for w in inventory["windows"] if w["pid"] == p and w["title"].startswith(_native._studio._base.PREFIX)) for p in self.pids]
            self.points = [[w["client"]["left"] + 120, w["client"]["top"] + 100] for w in windows]
        finally:
            client.close()

    def probes(self):
        return [read_json(self.run / f"probe-{i}.json") for i in range(self.count)]

    @staticmethod
    def assert_event_contract(steps, events):
        assert len(events) <= len(steps), "UI script emitted more observations than requested steps"
        for step_spec, event in zip(steps, events):
            if step_spec.get("expect_disabled"):
                assert event == {
                    "button": step_spec["button"],
                    "clicked": False,
                    "enabled": False,
                    "visible": True,
                    "observed_disabled": True,
                }, f"expected a visible disabled observation for {step_spec['button']}: {event}"
            else:
                assert event.get("button") == step_spec["button"] and event.get("clicked") is True, \
                    f"expected an accepted click for {step_spec['button']}: {event}"

    def execute(self, profile, steps, verify):
        put(self.run / "profile.json", profile)
        put(self.run / "ui.json", steps)
        process = subprocess.Popen([str(self.images[0]), "gui", "--profile", str(self.run / "profile.json"), "--ui-script", str(self.run / "ui.json")], cwd=self.run if self.offline else Path(__file__).resolve().parent.parent, creationflags=subprocess.CREATE_NO_WINDOW)
        self.owned.append(process)
        deadline, count, changed, changed_wall = time.monotonic() + 300, 0, time.monotonic(), time.time()
        try:
            while time.monotonic() < deadline:
                assert process.poll() is None, "GUI exited prematurely"
                try:
                    events = [json.loads(line) for line in (self.run / "ui.events.jsonl").read_text(encoding="utf-8").splitlines()]
                    if len(events) != count:
                        count, changed, changed_wall = len(events), time.monotonic(), time.time()
                    self.assert_event_contract(steps, events)
                    assert time.monotonic() - changed < 35, f"No accepted control/sample after {count}/{len(steps)}"
                    ready = read_json(self.run / "ui.ready.json")
                    progress = read_json(self.run / "ui.progress.json")
                    if not ready:
                        time.sleep(.05)
                        continue
                    graph = read_json(self.run / "ui.graph.json") if (self.run / "ui.graph.json").exists() else None
                    state = read_json(self.run / "ui.state.json") if (self.run / "ui.state.json").exists() else []
                    acks = [json.loads(line) for line in (self.run / "ui.graph.acks.jsonl").read_text(encoding="utf-8").splitlines()] if (self.run / "ui.graph.acks.jsonl").exists() else []
                    sample = {"graph": graph, "state": state, "acks": acks, "probes": self.probes(), "progress": progress}
                    if not count or time.monotonic() - changed < .35:
                        time.sleep(.05)
                        continue
                    name = steps[count - 1]["id"]
                    target = steps[count - 1]
                    if name in self.evidence["samples"]:
                        time.sleep(.05)
                        continue
                    if target.get("expect_disabled"):
                        sample["expected_disabled_control"] = target["button"]
                        if name in ("blocked-apply", "blocked-start"):
                            assert graph and graph["unresolved"], "blocked graph control lacks unresolved-ID warning"
                            assert not (self.run / "ui.state.json").exists(), "offline gate unexpectedly created a routing worker state"
                    if not verify(name, sample):
                        time.sleep(.05)
                        continue
                    if (name == "connect-a-b" and self.offline) or (name in ("apply", "graph-page") and self.count and graph and graph["draft"]["edges"]):
                        png = self.run / "ui.png"
                        if not png.exists() or png.stat().st_mtime < changed_wall:
                            time.sleep(.05)
                            continue
                        with Image.open(png) as image:
                            image.load()
                            image.save(self.run / "graph-canvas.png")
                        sample["owned_graph_screenshot"] = str(self.run / "graph-canvas.png")
                    self.evidence["samples"][name] = sample
                    self.evidence["clicked_events"] = events
                    self.evidence["ui_events"] = events
                    self.evidence["disabled_control_not_clicked"] = any(e.get("observed_disabled") for e in events)
                    self.save()
                    (self.run / "ui.sampled.json").write_text(str(count), encoding="utf-8")
                    print(f"UI accepted {name}: {events[-1]['button']}", flush=True)
                    if count == len(steps):
                        self.evidence["result"] = "PASS"
                        print(f"PASS: {count} actual controls. Evidence: {self.run}", flush=True)
                        return
                except (OSError, ValueError):
                    time.sleep(.05)  # App-local telemetry/PNG can be read mid-write.
            raise AssertionError("Finite graph UI deadline exhausted")
        except Exception as error:
            self.evidence["failure"] = repr(error)
            print(f"NOT ACCEPTED: {error!a}. Evidence: {self.run}", flush=True)
            raise
        finally:
            for owned in reversed(self.owned):
                if owned.poll() is None:
                    owned.terminate()
                owned.wait(timeout=15)
            self.evidence["owned_processes_closed"] = all(p.poll() is not None for p in self.owned)
            self.save()


def offline(args):
    gate = UiGate(args, "offline", offline=True)
    profile = {"version": 1, "concurrent": True, "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": []},
        "seats": [{"name": f"Legacy {letter}", "mouse": f"virtual://mouse/{21+i}", "keyboard": f"virtual://keyboard/{21+i}" if i < 3 else None} for i, letter in enumerate("ABCD")]}
    steps = [step("profiles", "Profiles"), step("save-legacy", "Save profile", {"profile_path": gate.run / "normalized.json"}),
        step("reload-materialized", "Load profile", {"profile_path": gate.run / "normalized.json"}), step("save-again", "Save profile", {"profile_path": gate.run / "again.json"}),
        step("graph-page", "Replication Graph"), step("node-a", "Graph user 1"), step("node-b", "Graph user 2"),
        step("connect-a-b", "Connect selected users", {"graph_mouse": "true", "graph_keyboard": "true", "graph_text": "true"}), step("wire-a-b", "Graph wire 1"),
        step("update-keyboard-only", "Update selected connection", {"graph_mouse": "false", "graph_keyboard": "true", "graph_text": "false"}),
        step("undo-update", "Undo graph edit"), step("redo-update", "Redo graph edit"), step("accessible-edge", "Graph connection 1"), step("remove-edge", "Remove selected connection"),
        step("undo-remove", "Undo graph edit"), step("clear", "Clear replication graph"), step("undo-clear", "Undo graph edit"),
        step("bad-file", "Load graph JSON", {"graph_file_path": gate.run / "bad.json"}), step("no-keyboard", "Load graph JSON", {"graph_file_path": gate.run / "no-keyboard.json"}),
        step("undo-after-failures", "Undo graph edit"), step("redo-after-failures", "Redo graph edit"),
        step("save-graph", "Save graph JSON", {"graph_file_path": gate.run / "graph.json"}), step("apply-offline", "Apply replication graph"),
        step("profile-committed-page", "Profiles"), step("save-committed", "Save profile", {"profile_path": gate.run / "committed.json"}),
        step("configure-name-device", "Configure user", {"selected_user": 0, "seat_name": "Renamed A", "seat_mouse": "virtual://mouse/41", "seat_keyboard": "virtual://keyboard/41"}),
        step("name-device-input-tab", "Input"), step("renamed-profile-page", "Profiles"), step("save-renamed", "Save profile", {"profile_path": gate.run / "renamed.json"}),
        step("load-reordered", "Load profile", {"profile_path": gate.run / "reordered.json"}), step("save-reordered", "Save profile", {"profile_path": gate.run / "reordered-saved.json"}),
        step("reordered-graph", "Replication Graph"), step("reordered-node-a", "Graph user 3"),
        step("remove-user-page", "Configure user", {"selected_user": 2}), step("remove-user", "Remove this user"), step("missing-page", "Replication Graph"),
        step("save-dangling-graph", "Save graph JSON", {"graph_file_path": gate.run / "dangling.json"}), step("blocked-apply", "Apply replication graph", disabled=True)]
    ids, all_edge, key_edge = [], None, None
    def verify(name, sample):
        nonlocal ids, all_edge, key_edge
        graph = sample["graph"]
        if name == "save-legacy":
            value = read_json(gate.run / "normalized.json")
            ids = [s["id"] for s in value["seats"]]
            assert len(set(ids)) == 4 and all(ids)
            assert [s["mouse"] for s in value["seats"]] == [s["mouse"] for s in profile["seats"]]
            all_edge = _native.definition([_native.edge(ids[0], ids[1])])
            key_edge = _native.definition([_native.edge(ids[0], ids[1], mouse=False, text=False)])
            (gate.run / "bad.json").write_text('{"version":1,"edges":', encoding="utf-8")
            put(gate.run / "no-keyboard.json", _native.definition([_native.edge(ids[0], ids[3], mouse=False, text=False)]))
            sample["profile"] = value
        if name == "save-again":
            assert read_json(gate.run / "again.json") == read_json(gate.run / "normalized.json")
        if name == "node-a":
            assert graph["selected_source"] == ids[0]
        if name == "node-b":
            assert graph["selected_source"] == ids[0] and graph["selected_target"] == ids[1]
        if name in ("connect-a-b", "undo-update", "undo-after-failures"):
            if graph["draft"] != all_edge:
                return False
        if name in ("update-keyboard-only", "redo-update", "undo-remove", "undo-clear", "redo-after-failures"):
            if graph["draft"] != key_edge:
                return False
        if name in ("wire-a-b", "accessible-edge"):
            assert graph["selected_edge"] == 0 and graph["selected_source"] == ids[0] and graph["selected_target"] == ids[1]
        if name in ("remove-edge", "clear"):
            assert graph["draft"] == _native.definition([])
        if name in ("bad-file", "no-keyboard"):
            if not graph["error"]:
                return False
            before = gate.evidence["samples"]["undo-clear"]["graph"]
            assert all(graph[field] == before[field] for field in ("draft", "applied", "undo_available", "redo_available", "pending", "graph_revision")), graph
        if name == "save-graph":
            assert read_json(gate.run / "graph.json") == key_edge
        if name == "apply-offline":
            assert graph["applied"] == key_edge and not graph["pending"]
        if name == "save-committed":
            value = read_json(gate.run / "committed.json")
            assert value["replication"] == key_edge and [s["id"] for s in value["seats"]] == ids
        if name == "save-renamed":
            value = read_json(gate.run / "renamed.json")
            assert value["seats"][0]["name"] == "Renamed A" and value["seats"][0]["mouse"] == "virtual://mouse/41" and value["seats"][0]["keyboard"] == "virtual://keyboard/41"
            assert [s["id"] for s in value["seats"]] == ids and value["replication"] == key_edge
            reordered = copy.deepcopy(value)
            reordered["seats"] = [reordered["seats"][i] for i in (3, 1, 0, 2)]
            put(gate.run / "reordered.json", reordered)
        if name == "save-reordered":
            value = read_json(gate.run / "reordered-saved.json")
            assert value == read_json(gate.run / "reordered.json") and value["replication"] == key_edge
        if name == "reordered-node-a":
            assert graph["selected_source"] == ids[0]
        if name in ("missing-page", "save-dangling-graph"):
            assert graph["draft"] == key_edge and graph["unresolved"] and ids[0] in graph["unresolved"]
            if name == "save-dangling-graph":
                assert read_json(gate.run / "dangling.json") == key_edge
        return True
    gate.execute(profile, steps, verify)
    offline_start(args, read_json(gate.run / "reordered-saved.json"), ids[0])


def offline_tail(args, prefix):
    """Retain accepted prefix evidence; retest only blocked controls."""
    profile = read_json(prefix / "reordered-saved.json")
    source_id = profile["replication"]["edges"][0]["source"]
    selected = next(i for i, seat in enumerate(profile["seats"]) if seat["id"] == source_id)
    gate = UiGate(args, "offline-blocked-apply", offline=True)
    steps = [step("remove-page", "Configure user", {"selected_user": selected}), step("remove-user", "Remove this user"),
             step("missing-warning", "Replication Graph"), step("blocked-apply", "Apply replication graph", disabled=True)]
    def verify(name, sample):
        if name == "missing-warning":
            assert sample["graph"]["draft"] == profile["replication"] and source_id in sample["graph"]["unresolved"]
        return True
    gate.execute(profile, steps, verify)
    offline_start(args, profile, source_id)


def offline_start(args, reordered, source_id):
    # A separate tiny app session proves actual disabled Start without skipping
    # an expected-disabled Apply in the UI event stream.
    start_gate = UiGate(args, "offline-blocked-start", offline=True)
    reordered = copy.deepcopy(reordered)
    # Explicit IDs permit UTF-8. Byte 16 of this ID is inside the emoji;
    # missing-endpoint rendering must use character-safe truncation.
    unicode_id = "seat-日本語😀A"
    for seat in reordered["seats"]:
        if seat["id"] == source_id:
            seat["id"] = unicode_id
    for connection in reordered["replication"]["edges"]:
        if connection["source"] == source_id:
            connection["source"] = unicode_id
        if connection["target"] == source_id:
            connection["target"] = unicode_id
    unicode_graph = reordered["replication"]
    start_steps = [step("remove-page", "Configure user", {"selected_user": 2}), step("remove-user", "Remove this user"),
                   step("missing-warning", "Replication Graph"), step("input-page", "Input lab"), step("blocked-start", "Start routing", disabled=True)]
    def verify_start(name, sample):
        if name == "missing-warning":
            assert sample["graph"]["draft"] == unicode_graph and sample["graph"]["unresolved"] and unicode_id in sample["graph"]["unresolved"]
        return True
    start_gate.execute(reordered, start_steps, verify_start)


def canvas_only(args, profile_path):
    gate = UiGate(args, "direction-canvas", offline=True)
    profile = read_json(profile_path)
    def verify(name, sample):
        assert sample["graph"]["draft"] == profile["replication"]
        if name == "connect-a-b":
            assert sample["graph"]["selected_source"] == profile["seats"][0]["id"]
        return True
    gate.execute(profile, [step("graph-page", "Replication Graph"), step("connect-a-b", "Graph user 1")], verify)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--offline-tail", type=Path)
    parser.add_argument("--canvas-only", type=Path)
    args = parser.parse_args()
    assert args.offline or args.offline_tail or args.canvas_only, "Choose an explicit finite variant; live persistence runner is separate."
    if args.canvas_only:
        canvas_only(args, args.canvas_only)
    elif args.offline_tail:
        offline_tail(args, args.offline_tail)
    else:
        offline(args)


if __name__ == "__main__":
    main()
