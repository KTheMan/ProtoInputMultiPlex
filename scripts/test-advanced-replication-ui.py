"""Finite actual Ouroboros advanced-edge/queue gate on two owned targets.

Requires producer READY. Uses only app-local scripted fields and genuine control
responses, native owned EDIT readback and the real configured queue capacity.
"""
import argparse
import copy
import importlib.util
from pathlib import Path
import shutil
import time
from PIL import Image

_spec = importlib.util.spec_from_file_location("advanced_ui_helpers", Path(__file__).with_name("test-replication-graph-ui.py"))
_ui = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_ui)
_spec = importlib.util.spec_from_file_location("advanced_native_helpers", Path(__file__).with_name("test-advanced-replication-acceptance.py"))
_advanced = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_advanced)
UiGate, step, put, read_json = _ui.UiGate, _ui.step, _ui.put, _ui.read_json
s = _ui._native._studio


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--ready", action="store_true")
    args = parser.parse_args()
    assert args.ready, "No live launch before producer READY"
    gate = UiGate(args, "advanced-queue", count=2)
    gate.fixtures()
    ids = ["advanced-ui-source", "advanced-ui-dest"]
    profile = {"version": 1, "concurrent": True, "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": gate.pids},
        "seats": [{"id": ids[i], "name": "Advanced " + ("Source" if i == 0 else "Destination"), "mouse": f"virtual://mouse/{i+1}", "keyboard": f"virtual://keyboard/{i+1}", "desktop_start": gate.points[i], "cursor": {"lock_window": False}} for i in range(2)]}
    options = {**copy.deepcopy(_advanced.DEFAULT_OPTIONS), "position": "source_relative", "offset_x": gate.points[1][0] - gate.points[0][0], "offset_y": gate.points[1][1] - gate.points[0][1],
               "motion": False, "wheel": False, "horizontal_wheel": False, "buttons": [False, True, False, True, True], "click_delay_ms": 60000, "step_interval_ms": 250}
    definition = _ui._native.definition([_advanced.edge(ids[0], ids[1], options)])
    invalid = copy.deepcopy(definition)
    invalid["edges"][0]["options"]["click_delay_ms"] = 60001
    put(gate.run / "bad-options.json", invalid)
    holding = s.clip(s.event(0, s.key(0, True)), s.event(0, s.button(0, True, code=3)), s.event(20_000_000, s.key(0, False)), s.event(20_000_000, s.button(0, False, code=3)))
    overflow = s.clip(*[s.event(0, s.button(0, i % 2 == 0, code=3)) for i in range(4096)], s.event(0, s.button(0, True, code=4)))
    put(gate.run / "holding.json", holding)
    put(gate.run / "overflow.json", overflow)
    staged = {"graph_source": 0, "graph_target": 1, "graph_mouse": "true", "graph_keyboard": "true", "graph_text": "true", "graph_position": "source_relative", "graph_offset_x": options["offset_x"], "graph_offset_y": options["offset_y"],
              "graph_click_delay_ms": 60000, "graph_step_interval_ms": 250}
    steps = [step("input-page", "Input lab"), step("start", "Start routing"), step("select-a", "1 Hold click"), step("drop-a", "1 Release click"), step("select-b", "2 Hold click"), step("drop-b", "2 Release click"),
             step("graph-page", "Replication Graph"), step("movement-control", "Movement"), step("vertical-control", "Vertical wheel"), step("horizontal-control", "Horizontal wheel"),
             step("primary-control", "Allow Primary"), step("middle-control", "Allow Middle"), step("connect", "Connect selected users", staged), step("apply", "Apply replication graph"),
             step("ordinary-input", "Input lab"), step("ordinary-b-hold", "Hold key", {"selected_user": 1, "key_name": "f6"}),
             step("studio-page", "Macro Studio"), step("studio-select", "Inspect user macro", {"studio_user": 0}), step("load-holding", "Load user clip JSON", {"studio_file_path": gate.run / "holding.json"}), step("play-holding", "Play user macro", {"studio_loops": 1, "studio_repeat": "false", "studio_keep_awake": "false"}),
             step("pending-page", "Replication Graph"), step("bad-options", "Load graph JSON", {"graph_file_path": gate.run / "bad-options.json"}), step("cancel-pending", "Cancel pending replication"),
             step("studio-cancel-page", "Macro Studio"), step("cancel-source", "Cancel user playback"), step("load-overflow", "Load user clip JSON", {"studio_file_path": gate.run / "overflow.json"}), step("play-overflow", "Play user macro"),
             step("overflow-page", "Replication Graph"), step("apply-resume", "Apply replication graph"), step("stop", "Stop routing"), step("reload-profiles", "Profiles"), step("save-profile", "Save profile", {"profile_path": gate.run / "advanced-profile.json"}), step("reload-profile", "Load profile", {"profile_path": gate.run / "advanced-profile.json"}), step("reload-graph", "Replication Graph")]
    overflow_baseline, overflow_clock = None, None
    def verify(name, sample):
        nonlocal overflow_baseline, overflow_clock
        graph, probes, state = sample["graph"], sample["probes"], sample["state"]
        if name in ("select-a", "select-b"):
            user = int(name == "select-b")
            if len(state) != 2 or state[user]["pid"] != gate.pids[user] or not probes[user]["buttons"] & 1:
                return False
        if name == "connect":
            assert graph["draft"] == definition and graph["applied"] == _ui._native.definition([])
        if name in ("apply", "apply-resume"):
            if graph["pending"] or graph["applied"] != definition:
                return False
            expected_revision = 1 if name == "apply" else 2
            assert graph["graph_revision"] == expected_revision
            assert sample["acks"][-1]["graph_revision"] == expected_revision and not sample["acks"][-1]["error"]
            if name == "apply-resume":
                assert not graph["replication"]["suspended"] and not graph["replication"]["error"]
                if any(r["held_keys"] or r["buttons"] for r in probes):
                    return False
        if name == "save-profile":
            assert read_json(gate.run / "advanced-profile.json")["replication"] == definition
        if name == "ordinary-b-hold" and 117 not in probes[1]["held_keys"]:
            return False
        if name == "play-holding":
            if not (116 in probes[0]["held_keys"] and probes[0]["buttons"] & 8 and 116 in probes[1]["held_keys"] and 117 in probes[1]["held_keys"]):
                return False
        if name == "pending-page":
            if graph["replication"]["pending_events"] != 1:
                return False
            assert not graph["replication"]["suspended"]
        if name == "bad-options":
            if not graph["error"]:
                return False
            before = gate.evidence["samples"]["pending-page"]["graph"]
            assert all(graph[k] == before[k] for k in ("draft", "applied", "undo_available", "redo_available", "graph_revision"))
            assert graph["replication"]["pending_events"] == 1
            assert 116 in probes[0]["held_keys"] and probes[0]["buttons"] & 8 and 117 in probes[1]["held_keys"]
        if name == "cancel-pending":
            if graph["replication"]["pending_events"] or 116 in probes[1]["held_keys"]:
                return False
            assert graph["draft"] == definition and graph["applied"] == definition and graph["graph_revision"] == 1
            assert 116 in probes[0]["held_keys"] and probes[0]["buttons"] & 8 and 117 in probes[1]["held_keys"]
        if name == "cancel-source":
            if probes[0]["held_keys"] or probes[0]["buttons"]:
                return False
            assert 117 in probes[1]["held_keys"]
        if name == "load-overflow":
            overflow_baseline = [r["mouse_event_count"] for r in probes]
            overflow_clock = time.time()
        if name == "overflow-page":
            queue = graph["replication"]
            if not queue["suspended"] or probes[0]["mouse_event_count"] != overflow_baseline[0] + 4096:
                return False
            assert queue["error"] and queue["pending_events"] == 0 and queue["pending_bytes"] == 0 and queue["capacity"] == 4096 and queue["cancelled"] >= 4096
            assert not probes[0]["buttons"] and not probes[0]["held_keys"] and probes[1]["mouse_event_count"] == overflow_baseline[1] and 117 in probes[1]["held_keys"]
            png = gate.run / "ui.png"
            events_path = gate.run / "ui.events.jsonl"
            if not png.exists() or png.stat().st_mtime < max(overflow_clock, events_path.stat().st_mtime):
                return False
            with Image.open(png) as image:
                image.load()
                image.save(gate.run / "overflow-caption.png")
            sample["owned_overflow_screenshot"] = str(gate.run / "overflow-caption.png")
        if name == "stop":
            if any(r["held_keys"] or r["buttons"] for r in probes):
                return False
            hwnds = [u["overlay"]["hwnd"] for u in gate.evidence["samples"]["play-holding"]["state"]]
            if any(_ui.user32.IsWindow(h) for h in hwnds):
                return False
        if name == "reload-graph":
            assert graph["draft"] == definition and graph["applied"] == definition and not graph["pending"]
        return True
    gate.execute(profile, steps, verify)


if __name__ == "__main__":
    main()
