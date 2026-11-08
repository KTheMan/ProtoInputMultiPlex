"""Bounded genuine Ouroboros generated-seat controls and profile cleanup gate.

Uses app-local pointer events, actual worker ACKs and native owned EDIT readback.
Run after producer READY, sequentially after the new native gate.
"""
import argparse
from ctypes import wintypes
import importlib.util
import json
from pathlib import Path
import subprocess
import time
from PIL import Image

_spec = importlib.util.spec_from_file_location("virtual_ui_helpers", Path(__file__).with_name("test-replication-graph-ui.py"))
_ui = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_ui)
UiGate, step, read_json = _ui.UiGate, _ui.step, _ui.read_json


class VirtualUiGate(UiGate):
    def fixtures(self):
        # Current discovery is versioned separately from the accepted historical
        # 17-tool graph helper. No old evidence or old-suite assertion is changed.
        for i in range(self.count):
            self.owned.append(subprocess.Popen([str(self.images[i % 2]), "probe", "--automation", "--report", str(self.run / f"probe-{i}.json")], creationflags=subprocess.CREATE_NO_WINDOW))
        self.pids = [p.pid for p in self.owned]
        client = _ui.Client(self.images[0], self.args.runtime.resolve(), self.run / "discovery.stderr.log")
        try:
            tools = client.rpc("tools/list", {})["tools"]
            assert {t["name"] for t in tools} == (_ui._native.GraphGate.tool_names | {"virtual_users"}) and len(tools) == 18
            assert all(t["inputSchema"]["type"] == "object" for t in tools)
            inventory = _ui.eventually(lambda: client.tool("routing_windows"), lambda v: all(any(w["pid"] == p and w["title"].startswith(_ui._native._studio._base.PREFIX) for w in v["windows"]) for p in self.pids), 60)
            for i, pid in enumerate(self.pids):
                window = next(w for w in inventory["windows"] if w["pid"] == pid and w["title"].startswith(_ui._native._studio._base.PREFIX))
                assert _ui.user32.SetWindowPos(window["hwnd"], wintypes.HWND(-1), 30 + i % 2 * 460, 40 + i // 2 * 410, 420, 360, 0x10)
            inventory = client.tool("routing_windows")
            windows = [next(w for w in inventory["windows"] if w["pid"] == pid and w["title"].startswith(_ui._native._studio._base.PREFIX)) for pid in self.pids]
            self.points = [[w["client"]["left"] + 120, w["client"]["top"] + 100] for w in windows]
            self.centers = [[(w["client"]["left"] + w["client"]["right"]) // 2, (w["client"]["top"] + w["client"]["bottom"]) // 2] for w in windows]
        finally:
            client.close()

    def virtual_sample(self, sample):
        sample["virtual_users"] = read_json(self.run / "ui.virtual-users.json")
        path = self.run / "ui.virtual-users.acks.jsonl"
        sample["virtual_acks"] = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()] if path.exists() else []
        return sample["virtual_users"]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--ready", action="store_true")
    parser.add_argument("--cleanup-tail", action="store_true", help="Focused Stop/profile tail after accepted lifecycle prefix")
    args = parser.parse_args()
    assert args.ready, "No live launch before producer READY"
    gate = VirtualUiGate(args, "virtual-users-cleanup" if args.cleanup_tail else "virtual-users", count=4)
    gate.fixtures()
    ids = ["virtual-ui-original-a", "virtual-ui-original-c"]
    original_edge = {"source": ids[1], "target": ids[0], "mouse": False, "keyboard": False, "text": True}
    profile = {"version": 1, "concurrent": True, "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": gate.pids},
        "replication": {"version": 1, "edges": [original_edge]},
        "seats": [{"id": ids[i], "name": ["Original UI A64", "Original UI C64"][i], "mouse": f"virtual://mouse/{i + 1}", "keyboard": f"virtual://keyboard/{i + 1}", "desktop_start": gate.points[physical], "cursor": {"size": 42, "badge": "own"}} for i, physical in enumerate((0, 2))]}
    def start_fields(physical, name):
        return {"clone_source_id": ids[0], "clone_target_pid": gate.pids[physical], "clone_name": name}
    steps = [step("input", "Input lab"), step("start", "Start routing"), step("select-a", "1 Hold click"), step("release-a", "1 Release click"), step("select-b", "2 Hold click"), step("release-b", "2 Release click"),
        step("ordinary-b", "Hold key", {"selected_user": 1, "key_name": "f6"}), step("ordinary-a", "Hold key", {"selected_user": 0, "key_name": "f5"}),
        step("virtual-page", "Virtual Replicas"), step("create-c", "Start virtual replica", start_fields(1, "Generated UI B32")), step("select-c", "Select replica 3"),
        step("duplicate-rejected", "Start virtual replica", start_fields(1, "Duplicate UI B32")), step("recover-create-d", "Start virtual replica", start_fields(3, "Generated UI D32")),
        step("select-d-image", "Select replica 4"), step("hold-d-image", "Select replica 4"),
        step("input-future", "Input lab"), step("source-f7", "Hold key", {"selected_user": 0, "key_name": "f7"}), step("source-text", "Send text", {"selected_user": 0, "text_input": "UI replica café 🙂"}), step("source-back", "1 Hold Back"),
        step("virtual-remove", "Virtual Replicas"), step("select-remove-c", "Select replica 3"), step("stop-c", "Stop selected virtual replica"), step("select-remove-d", "Select replica 4"), step("stop-d", "Stop selected virtual replica"),
        step("replacement", "Start virtual replica", start_fields(1, "Replacement UI B32")), step("select-replacement", "Select replica 5"), step("stop-session", "Stop routing"), step("profiles", "Profiles"),
        step("save-originals", "Save profile", {"profile_path": gate.run / "originals-only.json"}), step("reload-originals", "Load profile", {"profile_path": gate.run / "originals-only.json"}), step("stopped-virtual-page", "Virtual Replicas")]
    if args.cleanup_tail:
        steps = [steps[i] for i in (0, 1, 8, 9, 26, 27, 28, 29, 30)]
    initial_clone_mouse, screenshot_after = None, None
    def originals(probes):
        return 116 in probes[0]["held_keys"] and 117 in probes[2]["held_keys"]
    def verify(name, sample):
        nonlocal initial_clone_mouse, screenshot_after
        probes, state = sample["probes"], sample["state"]
        if name.startswith("select-") and name in ("select-a", "select-b"):
            physical = 0 if name == "select-a" else 2
            if not probes[physical]["buttons"] & 1:
                return False
        if name == "ordinary-b" and 117 not in probes[2]["held_keys"]:
            return False
        if name == "ordinary-a" and not originals(probes):
            return False
        if name == "virtual-page":
            virtual = gate.virtual_sample(sample)
            assert virtual["state"]["revision"] == 0 and len(virtual["state"]["slots"]) == 2
            initial_clone_mouse = probes[1]["mouse_event_count"]
        if name in ("create-c", "recover-create-d", "replacement", "stop-c", "stop-d", "duplicate-rejected"):
            virtual = gate.virtual_sample(sample)
            if virtual["pending"]:
                return False
            roster = virtual["state"]
            expected_revision = {"create-c": 1, "recover-create-d": 2, "stop-c": 3, "stop-d": 4, "replacement": 5, "duplicate-rejected": 1}[name]
            if roster["revision"] != expected_revision:
                return False
            ack = sample["virtual_acks"][-1]
            assert ack["revision"] == expected_revision
            if not args.cleanup_tail:
                assert originals(probes), "clone GUI lifecycle released originals"
            if name == "duplicate-rejected":
                assert ack["error"] and len(roster["slots"]) == 3
            else:
                assert ack["error"] is None
            if name == "create-c":
                if probes[1]["cursor"] != gate.centers[1]:
                    return False
                assert probes[1]["mouse_event_count"] == initial_clone_mouse and not probes[1]["held_keys"], "attachment simulated a click or inherited old hold"
                clone = roster["slots"][2]
                assert clone["seat"]["cursor"] == roster["slots"][0]["seat"]["cursor"] and clone["seat"]["annotation"]["tool"] == "off" and clone["seat"]["magnifier"]["mode"] == "off"
            if name in ("stop-c", "stop-d"):
                physical = 1 if name == "stop-c" else 3
                user = 2 if name == "stop-c" else 3
                assert not roster["slots"][user]["active"]
                if probes[physical]["held_keys"] or probes[physical]["buttons"]:
                    return False
                if name == "stop-c":
                    assert 118 in probes[3]["held_keys"] and probes[3]["buttons"] & 8, "removal released unrelated clone"
            if name == "replacement":
                assert len(roster["slots"]) == 5 and roster["slots"][4]["active"]
                assert len({s["seat"]["id"] for s in roster["slots"]}) == 5
                assert len({s["seat"]["mouse"] for s in roster["slots"]}) == 5
                if probes[1]["cursor"] != gate.centers[1]:
                    return False
                assert not probes[1]["held_keys"] and not probes[1]["buttons"]
        if name == "select-d-image":
            virtual = gate.virtual_sample(sample)
            assert virtual["selected_clone_id"] == virtual["state"]["slots"][3]["seat"]["id"]
            png = gate.run / "ui.png"
            if screenshot_after is None:
                screenshot_after = time.time()
            if not png.exists() or png.stat().st_mtime < screenshot_after:
                return False
            with Image.open(png) as image:
                image.load()
                image.save(gate.run / "virtual-replicas.png")
            sample["owned_virtual_replica_screenshot"] = str(gate.run / "virtual-replicas.png")
        if name == "source-f7" and not all(118 in probes[i]["held_keys"] for i in (0, 1, 3)):
            return False
        if name == "source-text":
            if not all(probes[i]["text"] == "UI replica café 🙂" for i in (0, 1, 3)):
                return False
            assert probes[2]["text"] == "" and originals(probes)
        if name == "source-back" and not all(probes[i]["buttons"] & 8 for i in (0, 1, 3)):
            return False
        if name == "stop-session":
            # ui.state.json is the last routing tick; the GUI does not promise a
            # rewrite on Stop. Independently query its old native HWNDs instead.
            if any(r["held_keys"] or r["buttons"] for r in probes):
                return False
            if any(_ui.user32.IsWindow(u["overlay"]["hwnd"]) for u in state):
                return False
            virtual = gate.virtual_sample(sample)
            if virtual["state"] is not None or virtual["selected_clone_id"] is not None or virtual["pending"]:
                return False
        if name == "save-originals":
            saved = read_json(gate.run / "originals-only.json")
            assert [s["id"] for s in saved["seats"]] == ids
            edges = saved["replication"]["edges"]
            assert [{k: e[k] for k in original_edge} for e in edges] == [original_edge], "original graph changed or ephemeral edges leaked into saved profile"
            assert all("Generated" not in s["name"] and "Replacement" not in s["name"] for s in saved["seats"])
        if name == "stopped-virtual-page":
            virtual = gate.virtual_sample(sample)
            assert virtual["state"] is None and virtual["selected_clone_id"] is None and not virtual["pending"]
        return True
    gate.execute(profile, steps, verify)


if __name__ == "__main__":
    main()
