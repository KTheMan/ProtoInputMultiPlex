"""Finite real Ouroboros Macro Studio controls; own virtual fixtures only.

Fields stage app-local widgets. Every action must be an enabled, visible, actual
button click accepted by the production UI. Worker ACKs, saved ordinary clips,
native EDIT readback and delivered messages provide the evidence.
"""
import argparse
from ctypes import wintypes
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import time
from PIL import Image

_spec = importlib.util.spec_from_file_location("studio_native", Path(__file__).with_name("test-macro-studio-acceptance.py"))
_native = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_native)
Client, eventually, user32 = _native._base.Client, _native.eventually, _native.user32


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--assigned-smoke", action="store_true")
    parser.add_argument("--assigned-capture-tail", action="store_true")
    args = parser.parse_args()
    if args.assigned_capture_tail:
        args.assigned_smoke = True
    prefix = "acceptance-assigned-macro-studio-ui" if args.assigned_smoke else "acceptance-macro-studio-ui"
    if args.assigned_capture_tail:
        prefix = "acceptance-assigned-macro-studio-capture-ui"
    run = args.runtime.resolve() / time.strftime(prefix + "-%Y%m%d-%H%M%S")
    run.mkdir(parents=True)
    owned, client = [], None
    evidence = {"result": "NOT ACCEPTED", "samples": {}, "scope": "Actual typed editor, generators, per-user playback/cancel, native x64/x86 readback and ACK-only clip save. Assigned is a bounded smoke."}
    try:
        images = [run / "control64.exe", run / "probe32.exe"]
        for source, image in zip((args.executable, args.x86_executable), images):
            shutil.copy2(source, image)
        evidence["build_sha256"] = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in images}
        for i, image in enumerate(images):
            owned.append(subprocess.Popen([str(image), "probe", "--automation", "--capture-pattern", str(0x286098 + i * 0x10101), "--report", str(run / f"probe-{i}.json")], creationflags=subprocess.CREATE_NO_WINDOW))
        client = Client(images[0], args.runtime.resolve(), run / "discovery.stderr.log")
        tools = client.rpc("tools/list", {})["tools"]
        assert {t["name"] for t in tools} == _native.StudioGate.tool_names and len(tools) == len(_native.StudioGate.tool_names)
        assert all(t["inputSchema"]["type"] == "object" for t in tools)
        pids = [p.pid for p in owned]
        inventory = eventually(lambda: client.tool("routing_windows"), lambda v: all(any(w["pid"] == p and w["title"].startswith(_native._base.PREFIX) for w in v["windows"]) for p in pids), 60)
        windows = [next(w for w in inventory["windows"] if w["pid"] == p and w["title"].startswith(_native._base.PREFIX)) for p in pids]
        for i, window in enumerate(windows):
            assert user32.SetWindowPos(window["hwnd"], wintypes.HWND(-1), 30 + i * 460, 40, 420, 360, 0x10)
        inventory = client.tool("routing_windows")
        windows = [next(w for w in inventory["windows"] if w["pid"] == p and w["title"].startswith(_native._base.PREFIX)) for p in pids]
        points = [[w["client"]["left"] + 120, w["client"]["top"] + 100] for w in windows]
        if args.assigned_smoke:
            points = [[(w["client"]["left"] + w["client"]["right"]) // 2, (w["client"]["top"] + w["client"]["bottom"]) // 2] for w in windows]
        client.close()
        client = None
        report = lambda i: json.loads((run / f"probe-{i}.json").read_text(encoding="utf-8"))
        profile = {"version": 1, "concurrent": True, "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": pids},
            "seats": [{"name": f"Macro UI {i + 1}", "mouse": f"virtual://mouse/{i + 1}", "keyboard": f"virtual://keyboard/{i + 1}", "desktop_start": points[i], "cursor": {"lock_window": False}} for i in range(2)]}
        if args.assigned_smoke:
            profile["desktop"] = None
            for i, seat in enumerate(profile["seats"]):
                seat["target_pid"] = pids[i]
        (run / "profile.json").write_text(json.dumps(profile), encoding="utf-8")
        initial = _native.clip(_native.event(0, _native.key(0, True)), _native.event(200_000, _native.text(0, "R")), _native.event(400_000, _native.key(0, False)))
        held = _native.clip(_native.event(0, _native.key(0, True)), _native.event(0, _native.button(0, True)), _native.event(20_000_000, _native.text(0, "late")))
        (run / "initial.json").write_text(json.dumps(initial), encoding="utf-8")
        (run / "held.json").write_text(json.dumps(held), encoding="utf-8")
        (run / "invalid.json").write_text(json.dumps(_native.clip(_native.event(0, _native.text(0, "bad\0")))), encoding="utf-8")
        steps = []
        def step(name, button, fields=None, wait=550):
            steps.append({"id": name, "button": button, "wait_ms": wait, "wait_for_sample": True, "fields": {k: str(v) for k, v in (fields or {}).items()}})
        step("input-page", "Input lab")
        step("start", "Start routing", wait=1800)
        for i in range(2):
            step(f"select-{i}", f"{i + 1} Hold click", wait=1000)
            step(f"drop-{i}", f"{i + 1} Release click")
        step("ordinary-other-hold", "Hold key", {"selected_user": 1, "key_name": "f6"})
        step("studio-page", "Macro Studio")
        step("inspect-0", "Inspect user macro", {"studio_user": 0})
        step("load-base", "Load user clip JSON", {"studio_file_path": run / "initial.json"})
        step("save-base", "Save user clip JSON", {"studio_file_path": run / "base-saved.json"})
        step("retime", "Retime selected event", {"studio_event_index": 1, "studio_at_micros": 0})
        typed = {"studio_action_type": "text", "studio_use_advanced_json": "false"}
        step("insert-typed", "Insert event", {**typed, "studio_at_micros": 100000, "studio_action_text": "X"})
        step("replace-typed", "Replace event", {**typed, "studio_event_index": 2, "studio_at_micros": 150000, "studio_action_text": "Y"})
        step("duplicate-selected", "Duplicate selected event", {"studio_event_index": 2, "studio_offset_micros": 500000})
        step("shift-range", "Shift event range", {"studio_range_start": 4, "studio_range_end": 5, "studio_shift_micros": 100000})
        step("delete-selected", "Delete selected event", {"studio_event_index": 4})
        step("duplicate-range", "Duplicate event range", {"studio_range_start": 1, "studio_range_end": 3, "studio_offset_micros": 500000})
        step("delete-range", "Delete event range", {"studio_range_start": 4, "studio_range_end": 6})
        step("bad-shift", "Shift event range", {"studio_range_start": 0, "studio_range_end": 1, "studio_shift_micros": -1})
        step("save-preserved", "Save user clip JSON", {"studio_file_path": run / "edited.json"})
        step("bad-load", "Load user clip JSON", {"studio_file_path": run / "invalid.json"})
        step("save-after-bad-load", "Save user clip JSON", {"studio_file_path": run / "after-invalid.json"})
        step("copy", "Copy source macro to destination user", {"studio_remap_from": 0, "studio_remap_to": 1})
        step("inspect-1", "Inspect user macro", {"studio_user": 1})
        step("save-copy", "Save user clip JSON", {"studio_file_path": run / "copy.json"})
        step("inspect-play-0", "Inspect user macro", {"studio_user": 0})
        step("play-edited-0", "Play user macro", {"studio_loops": 2, "studio_speed": 2, "studio_delay_ms": 100, "studio_keep_awake": "true", "studio_repeat": "false"}, wait=1700)
        step("inspect-play-1", "Inspect user macro", {"studio_user": 1})
        step("play-edited-1", "Play user macro", wait=1700)
        step("record-inspect", "Inspect user macro", {"studio_user": 0})
        step("record-start", "Start user recording")
        step("record-input-page", "Input lab")
        step("record-text", "Send text", {"selected_user": 0, "text_input": "Z🙂"})
        step("record-key", "Tap key", {"selected_user": 0, "key_name": "f7"})
        step("record-studio-page", "Macro Studio")
        step("record-stop", "Stop user recording")
        step("record-save", "Save user clip JSON", {"studio_file_path": run / "recorded.json"})
        generation = {"studio_user": 0, "studio_center_x": points[0][0], "studio_center_y": points[0][1], "studio_radius": 12, "studio_points": 4, "studio_duration_ms": 360, "studio_click": "none"}
        step("square-generate", "Generate user macro", {**generation, "studio_pattern": "square"})
        step("square-save", "Save user clip JSON", {"studio_file_path": run / "square.json"})
        step("square-play", "Play user macro", {"studio_loops": 1, "studio_speed": 1, "studio_delay_ms": 0}, wait=1200)
        step("jiggle-generate", "Generate user macro", {**generation, "studio_pattern": "jiggle", "studio_seed": 41})
        step("jiggle-save", "Save user clip JSON", {"studio_file_path": run / "jiggle.json"})
        step("jiggle-play", "Play user macro", wait=1200)
        if args.assigned_smoke:
            step("assigned-bounds-page", "Configure user", {"selected_user": 0})
            step("assigned-lock-tab", "Lock")
            step("assigned-root-window-bounds", "Window bounds")
            step("assigned-bounds-apply", "Apply cursor settings")
            step("assigned-back-studio", "Macro Studio")
        step("held-load-0", "Load user clip JSON", {"studio_user": 0, "studio_file_path": run / "held.json"})
        step("held-play-0", "Play user macro", {"studio_repeat": "true", "studio_keep_awake": "true", "studio_speed": 1, "studio_delay_ms": 0})
        step("held-inspect-1", "Inspect user macro", {"studio_user": 1})
        step("held-load-1", "Load user clip JSON", {"studio_file_path": run / "held.json"})
        step("held-play-1", "Play user macro")
        step("cancel-inspect-0", "Inspect user macro", {"studio_user": 0})
        step("cancel-0", "Cancel user playback")
        step("cancel-inspect-1", "Inspect user macro", {"studio_user": 1})
        step("cancel-1", "Cancel user playback")
        step("reset-1", "Reset user macro")
        step("reset-0", "Reset user macro", {"studio_user": 0})
        if args.assigned_smoke:
            step("assigned-capture-page", "Annotation")
            step("assigned-root-capture-kind", "Export selected target composition PNG", {"annotation_user": 0})
            step("assigned-root-capture", "Save annotation PNG", {"annotation_export_path": run / "assigned-root.png"})
        step("stop-page", "Input lab")
        step("stop", "Stop routing")
        if args.assigned_smoke:
            allowed = {"input-page", "start", "select-0", "drop-0", "select-1", "drop-1", "ordinary-other-hold", "studio-page", "inspect-0", "load-base", "retime", "insert-typed", "replace-typed", "save-preserved", "play-edited-0", "assigned-bounds-page", "assigned-lock-tab", "assigned-root-window-bounds", "assigned-bounds-apply", "assigned-back-studio", "held-load-0", "held-play-0", "held-inspect-1", "held-load-1", "held-play-1", "cancel-inspect-0", "cancel-0", "cancel-inspect-1", "cancel-1", "assigned-capture-page", "assigned-root-capture-kind", "assigned-root-capture", "stop-page", "stop"}
            steps[:] = [s for s in steps if s["id"] in allowed]
        if args.assigned_capture_tail:
            allowed = {"input-page", "start", "select-0", "drop-0", "select-1", "drop-1", "assigned-bounds-page", "assigned-lock-tab", "assigned-root-window-bounds", "assigned-bounds-apply", "assigned-capture-page", "assigned-root-capture-kind", "assigned-root-capture", "stop-page", "stop"}
            steps[:] = [s for s in steps if s["id"] in allowed]
        (run / "ui.json").write_text(json.dumps(steps), encoding="utf-8")
        owned.append(subprocess.Popen([str(images[0]), "gui", "--profile", str(run / "profile.json"), "--ui-script", str(run / "ui.json")], cwd=Path(__file__).resolve().parent.parent, creationflags=subprocess.CREATE_NO_WINDOW))
        deadline, last_count, changed = time.monotonic() + 360, 0, time.monotonic()
        changed_wall = time.time()
        while time.monotonic() < deadline:
            assert owned[-1].poll() is None, "GUI exited before completion"
            try:
                events = [json.loads(line) for line in (run / "ui.events.jsonl").read_text(encoding="utf-8").splitlines()]
                if len(events) != last_count:
                    last_count, changed = len(events), time.monotonic()
                    changed_wall = time.time()
                assert time.monotonic() - changed < 35, f"UI stalled after {last_count}/{len(steps)} controls"
                if not events or time.monotonic() - changed < .35 or not json.loads((run / "ui.ready.json").read_text(encoding="utf-8")):
                    time.sleep(.05)
                    continue
                name = steps[len(events) - 1]["id"]
                if name in evidence["samples"]:
                    time.sleep(.05)
                    continue
                assert all(e["clicked"] for e in events)
                state = json.loads((run / "ui.state.json").read_text(encoding="utf-8")) if (run / "ui.state.json").exists() else []
                studio = json.loads((run / "ui.macro-studio.json").read_text(encoding="utf-8")) if (run / "ui.macro-studio.json").exists() else None
                acks = [json.loads(line) for line in (run / "ui.macro-studio.acks.jsonl").read_text(encoding="utf-8").splitlines()] if (run / "ui.macro-studio.acks.jsonl").exists() else []
                probes = [report(i) for i in range(2)]
            except (OSError, ValueError):
                time.sleep(.05)
                continue
            samples = evidence["samples"]
            sample = {"state": state, "studio": studio, "acks": acks, "probes": probes, "button": events[-1]["button"]}
            def lane(i):
                return studio["users"][i]
            def latest_clip(i):
                for ack in reversed(acks):
                    if ack["users"][i]["clip"] is not None and ack["users"][i]["revision"] == lane(i)["revision"]:
                        return ack["users"][i]["clip"]
                return None
            if name.startswith("select-"):
                i = int(name[-1])
                if len(state) != 2 or state[i]["pid"] != pids[i] or probes[i]["buttons"] != 1:
                    continue
            if name == "ordinary-other-hold" and 117 not in probes[1]["held_keys"]:
                continue
            if name in ("load-base", "retime", "insert-typed", "replace-typed", "duplicate-selected", "shift-range", "delete-selected", "duplicate-range", "delete-range", "copy", "held-load-0", "held-load-1", "square-generate", "jiggle-generate"):
                previous = next((s["acks"] for s in reversed(list(samples.values())) if s["acks"]), [])
                if not acks or len(acks) <= len(previous) or acks[-1]["error"]:
                    continue
            if name in ("retime", "insert-typed", "replace-typed", "duplicate-selected", "shift-range", "delete-selected", "duplicate-range", "delete-range"):
                value = latest_clip(0)
                times = {"retime": [0, 0, 400000], "insert-typed": [0, 0, 100000, 400000], "replace-typed": [0, 0, 150000, 400000],
                    "duplicate-selected": [0, 0, 150000, 400000, 650000], "shift-range": [0, 0, 150000, 400000, 750000], "delete-selected": [0, 0, 150000, 400000],
                    "duplicate-range": [0, 0, 150000, 400000, 500000, 650000], "delete-range": [0, 0, 150000, 400000]}[name]
                assert value and [e["at_micros"] for e in value["events"]] == times, (name, value)
                assert 117 in probes[1]["held_keys"], "editor released unrelated manual hold"
            if name == "replace-typed":
                # The egui screenshot contains only this owned app's rendered
                # client. Wait for a fresh editor frame, never publish Input lab
                # window inventory from the generic final ui.png capture.
                screenshot = run / "ui.png"
                if not screenshot.exists() or screenshot.stat().st_mtime < changed_wall:
                    time.sleep(.05)
                    continue
                try:
                    with Image.open(screenshot) as image:
                        image.load()
                        image.save(run / "macro-studio-editor.png")
                        sample["screenshot"] = {"path": str(run / "macro-studio-editor.png"), "size": list(image.size), "scope": "Owned egui app client on Macro Studio typed authoring page."}
                except OSError:
                    time.sleep(.05)
                    continue
            if name == "bad-shift":
                if not acks or not acks[-1]["error"]:
                    continue
                assert lane(0)["revision"] == samples["delete-range"]["studio"]["users"][0]["revision"] and 117 in probes[1]["held_keys"]
            saves = {"save-base": "base-saved.json", "save-preserved": "edited.json", "save-after-bad-load": "after-invalid.json", "save-copy": "copy.json", "record-save": "recorded.json", "square-save": "square.json", "jiggle-save": "jiggle.json"}
            if name in saves:
                path = run / saves[name]
                if not path.exists():
                    continue
                value = json.loads(path.read_text(encoding="utf-8"))
                sample["saved_clip"] = value
                if name == "save-base":
                    assert value == initial
                if name == "save-preserved":
                    assert [e["at_micros"] for e in value["events"]] == [0, 0, 150000, 400000]
                    assert [e["action"].get("text") for e in value["events"]] == [None, "R", "Y", None]
                if name == "save-after-bad-load":
                    assert value == samples["save-preserved"]["saved_clip"], "invalid local load replaced ACK cache"
                if name == "save-copy":
                    expected = json.loads(json.dumps(samples["save-preserved"]["saved_clip"]))
                    for e in expected["events"]:
                        e["action"]["user"] = 1
                    assert value == expected, value
                if name == "record-save":
                    assert len(value["events"]) == 3 and all(e["action"]["user"] == 0 for e in value["events"])
                    assert [e["action"]["type"] for e in value["events"]] == ["text", "packet", "packet"]
                    assert value["events"][0]["action"]["text"] == "Z🙂"
                if name == "square-save":
                    cx, cy = points[0]
                    assert [[e["action"]["x"], e["action"]["y"]] for e in value["events"]] == [[cx - 12, cy - 12], [cx + 12, cy - 12], [cx + 12, cy + 12], [cx - 12, cy + 12], [cx - 12, cy - 12]]
                if name == "jiggle-save":
                    assert len(value["events"]) == 4 and all(e["action"]["type"] == "move_to" for e in value["events"])
            if name.startswith("play-edited-"):
                i = int(name[-1])
                if not studio or not lane(i)["playback"] or lane(i)["playback"]["state"] != "completed" or probes[i]["text"] != "RYRY":
                    continue
                assert lane(i)["playback"]["loops_completed"] == 2 and lane(i)["power"] is None
                if i == 0:
                    assert 117 in probes[1]["held_keys"]
                delivered = [(e["code"], e["pressed"]) for e in probes[i]["native_events"] if e["kind"] == "key" and e["code"] == 116]
                if delivered[-4:] != [(116, True), (116, False)] * 2:
                    continue  # Worker completion precedes queued native delivery/report sampling.
                if args.assigned_smoke:
                    assert probes[i]["cursor"] == points[i], (probes[i]["cursor"], points[i])
            if name == "assigned-bounds-apply":
                expected = windows[0]["client"]
                if not state or state[0]["movement_bounds"] != expected:
                    continue
                assert state[0]["bounds"] == expected
                if not args.assigned_capture_tail:
                    assert 117 in probes[1]["held_keys"]
            if name == "assigned-root-capture":
                path = run / "assigned-root.png"
                annotation_path = run / "ui.annotation.acks.jsonl"
                if not path.exists() or not annotation_path.exists():
                    continue
                try:
                    annotation_acks = [json.loads(line) for line in annotation_path.read_text(encoding="utf-8").splitlines()]
                    annotation = next((a for a in reversed(annotation_acks) if a["export"] and a["export"]["path"] == str(path)), None)
                except (OSError, ValueError):
                    continue  # Periodic telemetry can be sampled mid-write.
                if annotation is None:
                    continue
                assert annotation["export"]["hwnd"] == windows[0]["hwnd"] and annotation["export"]["pid"] == pids[0]
                expected = windows[0]["client"]
                with Image.open(path) as image:
                    assert image.size == (expected["right"] - expected["left"], expected["bottom"] - expected["top"])
                    assert image.getpixel((28, 38)) == (255, 255, 255, 255)
                    assert image.getpixel((60, 70)) == (40, 96, 152, 255)
                    sample["root_capture"] = {"state": annotation, "size": list(image.size), "path": str(path)}
            if name == "record-start" and (not studio or not lane(0)["recording"]):
                continue
            if name == "record-stop" and (not studio or lane(0)["recording"] or lane(0)["loaded_events"] != 3):
                continue
            if name in ("square-play", "jiggle-play"):
                if not studio or not lane(0)["playback"] or lane(0)["playback"]["state"] != "completed":
                    continue
                assert not probes[0]["held_keys"] and not probes[0]["buttons"] and lane(0)["power"] is None
            if name.startswith("held-play-"):
                i = int(name[-1])
                if not studio or 116 not in probes[i]["held_keys"] or not probes[i]["buttons"] & 1 or not lane(i)["power"]:
                    continue
                assert lane(i)["playback"]["state"] == "playing" and lane(i)["power"]["active"]
                if i == 1:
                    assert 116 in probes[0]["held_keys"] and probes[0]["buttons"] & 1 and lane(0)["power"]
            if name == "cancel-0":
                if probes[0]["held_keys"] or probes[0]["buttons"] or lane(0)["power"]:
                    continue
                assert 116 in probes[1]["held_keys"] and probes[1]["buttons"] & 1 and lane(1)["power"] and lane(1)["playback"]["state"] == "playing"
            if name == "cancel-1":
                if any(p["held_keys"] or p["buttons"] for p in probes) or any(u["power"] for u in studio["users"]):
                    continue
            if name.startswith("reset-"):
                i = int(name[-1])
                if not studio or lane(i)["loaded_events"] != 0:
                    continue
            if name == "stop":
                if any(p["held_keys"] or p["buttons"] for p in probes):
                    continue
                handles = [u["overlay"]["hwnd"] for u in samples["assigned-root-capture" if args.assigned_capture_tail else "held-play-1"]["state"]]
                if any(user32.IsWindow(h) for h in handles):
                    continue
            samples[name] = sample
            evidence["clicked_events"] = events
            (run / "evidence.json").write_text(json.dumps(evidence, indent=2, ensure_ascii=False), encoding="utf-8")
            (run / "ui.sampled.json").write_text(str(len(events)), encoding="utf-8")
            print(f"UI accepted {name}: {sample['button']}", flush=True)
            if name == "stop":
                assert len(samples) == len(steps)
                evidence["result"] = "PASS"
                print(f"PASS: {len(steps)} actual Macro Studio controls. Evidence: {run}", flush=True)
                break
        else:
            raise AssertionError("Macro Studio UI finite deadline exhausted")
    except Exception as error:
        evidence["failure"] = repr(error)
        print(f"NOT ACCEPTED: {error!a}. Evidence: {run}", flush=True)
        raise
    finally:
        if client:
            client.close()
        for process in reversed(owned):
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=15)
        evidence["owned_processes_closed"] = all(p.poll() is not None for p in owned)
        (run / "evidence.json").write_text(json.dumps(evidence, indent=2, ensure_ascii=False), encoding="utf-8")


if __name__ == "__main__":
    main()
