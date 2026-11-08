"""Bounded owned-target GUI graph persistence/restart and keyboard-boundary gates."""
import argparse
import copy
import importlib.util
from pathlib import Path

_spec = importlib.util.spec_from_file_location("graph_ui_helpers", Path(__file__).with_name("test-replication-graph-ui.py"))
_ui = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_ui)
UiGate, step, put, read_json = _ui.UiGate, _ui.step, _ui.put, _ui.read_json
_native = _ui._native


def setup_profile(gate, restored=None, mouse_only=False):
    ids = ["graph-seat-" + c for c in "abcd"[:gate.count]]
    profile = {"version": 1, "concurrent": True, "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": gate.pids},
        "seats": [{"id": ids[i], "name": "Persistent " + "ABCD"[i], "mouse": f"virtual://mouse/{i+1}", "keyboard": f"virtual://keyboard/{i+1}" if not mouse_only or i == 0 else None,
                   "desktop_start": gate.points[i], "cursor": {"lock_window": False}} for i in range(gate.count)]}
    order = list(range(gate.count))
    if restored:
        profile = copy.deepcopy(restored)
        order = [3, 1, 0, 2]
        profile["desktop"]["allowed_pids"] = gate.pids
        profile["seats"] = [profile["seats"][i] for i in order]
        for seat in profile["seats"]:
            physical = ids.index(seat["id"])
            seat["desktop_start"] = gate.points[physical]
            seat["name"] = "Renamed " + seat["name"]
    if mouse_only:
        profile["replication"] = _native.definition([_native.edge(ids[0], ids[1], keyboard=False, text=False)])
    return profile, ids, order


def live(args, restored=None, mouse_only=False):
    gate = UiGate(args, "mouse-only-live" if mouse_only else ("profile-restart" if restored else "profile-save"), count=2 if mouse_only else 4)
    gate.fixtures()
    profile, ids, order = setup_profile(gate, restored, mouse_only)
    source = order.index(0)
    expected_graph = profile["replication"] if mouse_only or restored else _native.definition([_native.edge(ids[a], ids[b]) for a, b in ((0, 1), (0, 2), (1, 3), (2, 3))])
    put(gate.run / "bad-graph.json", _native.definition([_native.edge(ids[0], ids[1], mouse=False, keyboard=True, text=False)]) if mouse_only else {"version": 1, "edges": "invalid-array"})
    steps = [step("input-page", "Input lab"), step("start", "Start routing")]
    for logical in range(gate.count):
        steps += [step(f"select-{logical}", f"{logical+1} Hold click"), step(f"drop-{logical}", f"{logical+1} Release click")]
    steps += [step("graph-page", "Replication Graph")]
    if not restored and not mouse_only:
        for number, (a, b) in enumerate(((0, 1), (0, 2), (1, 3), (2, 3))):
            steps.append(step("add-" + str(number), "Connect selected users", {"graph_source": a, "graph_target": b, "graph_mouse": "true", "graph_keyboard": "true", "graph_text": "true"}))
        steps.append(step("apply", "Apply replication graph"))
    steps += [step("held-input-page", "Input lab"), step("hold-f5", "Hold key", {"selected_user": source, "key_name": "f5"})]
    if mouse_only:
        steps.append(step("hold-back", f"{source+1} Hold Back"))
    steps += [step("error-graph-page", "Replication Graph"), step("bad-file", "Load graph JSON", {"graph_file_path": gate.run / "bad-graph.json"}),
              step("save-preserved", "Save graph JSON", {"graph_file_path": gate.run / "preserved.json"}), step("delivery-page", "Input lab")]
    if mouse_only:
        steps += [step("stop", "Stop routing")]
    else:
        steps += [step("release-f5", "Release key", {"selected_user": source, "key_name": "f5"}),
                  step("unicode", "Send text", {"selected_user": source, "text_input": "Persist café 🙂"}),
                  step("tap-f6", "Tap key", {"selected_user": source, "key_name": "f6"}),
                  step("back-down", f"{source+1} Hold Back"), step("back-up", f"{source+1} Release Back"),
                  step("motion", f"{source+1} Move to point", point=[gate.points[0][0]+7, gate.points[0][1]+3], user=source), step("stop", "Stop routing"),
                  step("profile-page", "Profiles"), step("save-profile", "Save profile", {"profile_path": gate.run / "saved-profile.json"})]
    baseline = None
    def verify(name, sample):
        nonlocal baseline
        graph, probes, state = sample["graph"], sample["probes"], sample["state"]
        if name.startswith("select-"):
            logical = int(name.split("-")[-1])
            physical = order[logical]
            if len(state) != gate.count or state[logical]["pid"] != gate.pids[physical] or not probes[physical]["buttons"] & 1:
                return False
        if name == "graph-page":
            if not graph:
                return False
            if restored or mouse_only:
                assert graph["draft"] == expected_graph and graph["applied"] == expected_graph
            baseline = [r["native_event_count"] for r in probes]
            assert all(r["cursor"] == gate.points[i] and not r["held_keys"] and not r["buttons"] for i, r in enumerate(probes)), "initial anchors must be independent before delivery baseline"
        if name.startswith("add-"):
            number = int(name[-1])
            assert graph["draft"]["edges"] == expected_graph["edges"][:number + 1]
            assert graph["applied"] == _native.definition([])
        if name == "apply":
            if not graph or graph["pending"] or graph["applied"] != expected_graph:
                return False
            before = gate.evidence["samples"]["graph-page"]["graph"]["graph_revision"]
            assert graph["graph_revision"] == before + 1
            assert sample["acks"] and sample["acks"][-1]["graph_revision"] == graph["graph_revision"] and not sample["acks"][-1]["error"]
        if name in ("hold-f5", "hold-back"):
            if mouse_only:
                if 116 not in probes[0]["held_keys"] or (name == "hold-back" and not all(r["buttons"] & 8 for r in probes)):
                    return False
                assert not probes[1]["held_keys"]
            elif not all(116 in r["held_keys"] for r in probes):
                return False
        if name == "bad-file":
            if not graph or not graph["error"]:
                return False
            before = gate.evidence["samples"]["error-graph-page"]["graph"]
            assert all(graph[k] == before[k] for k in ("draft", "applied", "undo_available", "redo_available", "pending", "graph_revision"))
            assert 116 in probes[0]["held_keys"] if mouse_only else all(116 in r["held_keys"] for r in probes)
            if mouse_only:
                assert all(r["buttons"] & 8 for r in probes)
        if name == "save-preserved":
            assert read_json(gate.run / "preserved.json") == expected_graph
        if name == "release-f5" and any(116 in r["held_keys"] for r in probes):
            return False
        if name == "unicode" and not all(r["text"] == "Persist café 🙂" for r in probes):
            return False
        if name == "tap-f6":
            if not all([(e["vk"], e["pressed"]) for e in r["key_events"][-2:]] == [(117, True), (117, False)] for r in probes):
                return False
        if name == "back-down" and not all(r["buttons"] & 8 for r in probes):
            return False
        if name == "back-up" and any(r["buttons"] for r in probes):
            return False
        if name == "motion":
            if not all(r["cursor"] == [gate.points[i][0] + 7, gate.points[i][1] + 3] for i, r in enumerate(probes)):
                return False
            for i, r in enumerate(probes):
                assert r["native_event_count"] == len(r["native_events"])
                trace = r["native_events"][baseline[i]:]
                meaningful = [(e["kind"], e["code"], e["pressed"]) for e in trace if e["kind"] != "motion"]
                expected = [("key", 116, True), ("key", 116, False), *[("char", c, None) for c in _native._studio.utf16("Persist café 🙂")], ("key", 117, True), ("key", 117, False), ("button", 3, True), ("button", 3, False)]
                assert meaningful == expected, (i, meaningful)
        if name == "stop":
            if any(r["held_keys"] or r["buttons"] for r in probes):
                return False
            hwnds = [u["overlay"]["hwnd"] for u in gate.evidence["samples"]["held-input-page"]["state"]]
            if any(_ui.user32.IsWindow(h) for h in hwnds):
                return False
        if name == "save-profile":
            value = read_json(gate.run / "saved-profile.json")
            assert value["replication"] == expected_graph and [s["id"] for s in value["seats"]] == [profile["seats"][i]["id"] for i in range(4)]
            sample["saved_profile"] = value
        return True
    gate.execute(profile, steps, verify)
    return read_json(gate.run / "saved-profile.json") if not mouse_only else None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--mouse-only", action="store_true")
    args = parser.parse_args()
    if args.mouse_only:
        live(args, mouse_only=True)
    else:
        saved = live(args)
        live(args, restored=saved)


if __name__ == "__main__":
    main()
