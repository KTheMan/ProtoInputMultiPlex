"""Finite acceptance of actual Ouroboros controls using egui-local virtual clicks."""
import argparse
import ctypes
from ctypes import wintypes
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import time

_spec = importlib.util.spec_from_file_location("acceptance", Path(__file__).with_name("test-automation-acceptance.py"))
_module = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)
Client, eventually, user32 = _module.Client, _module.eventually, _module.user32


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--settings", action="store_true", help="Run the separate cursor settings group")
    parser.add_argument("--assigned", action="store_true", help="Bounded assigned-process settings smoke; requires --settings")
    parser.add_argument("--clip-boundary", action="store_true", help="Six-control macro cache rejection boundary")
    parser.add_argument("--appearance", action="store_true", help="Appearance/bounds controls and pause/resume alignment")
    parser.add_argument("--appearance-tail", action="store_true", help="Only the appearance/bounds lifecycle tail; requires --appearance")
    args = parser.parse_args()
    assert not args.assigned or args.settings or args.appearance
    assert not args.clip_boundary or not (args.settings or args.appearance)
    assert not (args.settings and args.appearance)
    assert not args.appearance_tail or args.appearance
    root = Path(__file__).resolve().parent.parent
    run_prefix = "acceptance-clip-boundary-ui" if args.clip_boundary else ("acceptance-assigned-settings-ui" if args.assigned else ("acceptance-settings-ui" if args.settings else "acceptance-ui"))
    if args.appearance:
        run_prefix = "acceptance-assigned-appearance-ui" if args.assigned else "acceptance-appearance-ui"
        appearance_spec = importlib.util.spec_from_file_location("appearance_gate", Path(__file__).with_name("test-appearance-acceptance.py"))
        appearance_module = importlib.util.module_from_spec(appearance_spec)
        appearance_spec.loader.exec_module(appearance_module)
    run = args.runtime.resolve() / time.strftime(run_prefix + "-%Y%m%d-%H%M%S")
    run.mkdir(parents=True)
    evidence = {"samples": {}, "result": "NOT ACCEPTED"}
    owned, client = [], None
    try:
        image, image32 = run / "control64.exe", run / "probe32.exe"
        shutil.copy2(args.executable, image)
        shutil.copy2(args.x86_executable, image32)
        for index, fixture in enumerate((image, image32)):
            owned.append(subprocess.Popen([str(fixture), "probe", "--automation", "--report", str(run / f"probe-{index}.json")], creationflags=subprocess.CREATE_NO_WINDOW))
        client = Client(image, args.runtime.resolve(), run / "discovery.stderr.log")
        pids = [p.pid for p in owned]
        inventory = eventually(lambda: client.tool("routing_windows"), lambda v: all(any(w["pid"] == p and w["title"].startswith(_module.PREFIX) for w in v["windows"]) for p in pids), 60)
        windows = [next(w for w in inventory["windows"] if w["pid"] == p and w["title"].startswith(_module.PREFIX)) for p in pids]
        for index, window in enumerate(windows):
            assert user32.SetWindowPos(window["hwnd"], wintypes.HWND(-1), 30 + index * 460, 40, 420, 360, 0x10)
        inventory = client.tool("routing_windows")
        windows = [next(w for w in inventory["windows"] if w["pid"] == p and w["title"].startswith(_module.PREFIX)) for p in pids]
        points = [[w["client"]["left"] + 120, w["client"]["top"] + 100] for w in windows]
        if args.appearance:
            displays = client.tool("routing_displays")
            monitor = next(m for m in displays["monitors"] if m["stable_id"] and m["primary"])
            anchor = points[0] if not args.assigned else [(windows[0]["client"]["left"] + windows[0]["client"]["right"]) // 2,
                                                        (windows[0]["client"]["top"] + windows[0]["client"]["bottom"]) // 2]
            first_region = {"left": anchor[0]+4, "top": anchor[1]+4, "right": anchor[0]+28, "bottom": anchor[1]+28}
            paused_region = {"left": anchor[0]+12, "top": anchor[1]+12, "right": anchor[0]+28, "bottom": anchor[1]+28}
        client.close()
        client = None
        profile = {"version": 1, "concurrent": True, "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": pids},
            "seats": [{"name": f"UI{64 if i == 0 else 32}", "mouse": f"virtual://mouse/{i+1}", "keyboard": f"virtual://keyboard/{i+1}",
                       "desktop_start": points[i], "cursor": {"lock_window": False}} for i in range(2)]}
        if args.assigned:
            profile["desktop"] = None
            for index, seat in enumerate(profile["seats"]):
                seat["target_pid"] = pids[index]
        (run / "profile.json").write_text(json.dumps(profile), encoding="utf-8")
        recorded = run / "ui-recorded.json"
        held = run / "ui-held.json"
        held.write_text(json.dumps({"version": 1, "events": [
            {"at_micros": 0, "action": {"type": "packet", "user": 0, "packet": {"device": 0xF1000003, "kind": 1, "flags": 0, "x": 63, "y": 0, "data": 116}}},
            {"at_micros": 0, "action": {"type": "packet", "user": 0, "packet": {"device": 0xF1000002, "kind": 0, "flags": 1, "x": 0, "y": 0, "data": 0}}},
            {"at_micros": 10_000_000, "action": {"type": "text", "user": 0, "text": "late-ui"}}]}), encoding="utf-8")
        steps = []
        def step(name, button, fields=None, wait=400):
            steps.append({"id": name, "button": button, "wait_ms": wait, "wait_for_sample": True, "fields": {k: str(v) for k, v in (fields or {}).items()}})
        step("start", "Start routing", wait=1800)
        for index in range(2):
            step(f"select-{index}", f"{index+1} Hold click", wait=1400)
            step(f"drop-{index}", f"{index+1} Release click")
        step("key64", "Tap key", {"selected_user": 0, "key_name": "f5"})
        step("hold32", "Hold key", {"selected_user": 1, "key_name": "f6"})
        step("release32", "Release key", {"selected_user": 1, "key_name": "f6"})
        step("text64", "Send text", {"selected_user": 0, "text_input": "UI64 東京 🦊"})
        step("text32", "Send text", {"selected_user": 1, "text_input": "UI32 café 🙂"})
        step("macro-page", "Macros & replication")
        step("record-start", "Start recording")
        step("record-input-page", "Input lab")
        step("record-text", "Send text", {"selected_user": 0, "text_input": "R"})
        step("record-key", "Tap key", {"selected_user": 0, "key_name": "f5"})
        step("record-page", "Macros & replication")
        step("record-stop", "Stop recording")
        step("save", "Save macro", {"macro_path": recorded})
        step("load", "Load macro", {"macro_path": recorded})
        step("play", "Play macro", {"macro_loops": 2, "macro_speed": 4}, wait=500)
        step("edge", "Add replication edge", {"replication_source": 0, "replication_target": 1, "replication_mouse": "false", "replication_keyboard": "true", "replication_text": "true"})
        step("apply", "Apply replication")
        step("replication-input-page", "Input lab")
        step("replicated-key", "Tap key", {"selected_user": 0, "key_name": "f5"})
        step("replicated-text", "Send text", {"selected_user": 0, "text_input": "Z"})
        step("cleanup-page", "Macros & replication")
        step("remove-edge", "Remove edge")
        step("apply-empty", "Apply replication")
        step("load-held", "Load macro", {"macro_path": held})
        step("play-held", "Play macro", {"macro_loops": 1, "macro_speed": 1})
        step("cancel", "Cancel playback", wait=400)
        step("stop", "Stop routing")
        if args.settings:
            steps.clear()
            defaults_file, configured_file = run / "legacy-defaults.json", run / "configured-profile.json"
            step("defaults-page", "Profiles")
            step("save-defaults", "Save profile", {"profile_path": defaults_file})
            step("setup-lab", "Input lab")
            step("start", "Start routing", wait=1800)
            for index in range(2):
                step(f"select-{index}", f"{index+1} Hold click", wait=1400)
                step(f"drop-{index}", f"{index+1} Release click")
            step("settings-held", "Hold all users")
            step("invalid-text-held", "Send text", {"selected_user": 0, "text_input": "A" * 4097})
            step("motion-user", "Configure user", {"selected_user": 0})
            step("motion-tab", "Motion")
            step("strong", "Strong response")
            step("apply-held", "Apply cursor settings")
            step("motion-lab", "Input lab")
            step("strong-motion", "1 Right")
            step("wheel-user", "Configure user", {"selected_user": 0})
            step("wheel-tab", "Wheel")
            step("convert", "Convert to vertical", {"horizontal_wheel_scale": 2, "invert_horizontal_wheel": "true"}, wait=3500)
            step("apply-wheel", "Apply cursor settings", {"horizontal_wheel_scale": 2, "invert_horizontal_wheel": "true"})
            step("wheel-lab", "Input lab")
            step("converted-wheel", "1 Horizontal wheel")
            step("buttons-user", "Configure user", {"selected_user": 0})
            step("buttons-tab", "Buttons")
            step("apply-map", "Apply cursor settings", {"button_mapping_0": "back", "button_mapping_3": "disabled"})
            step("mapped-lab", "Input lab")
            step("mapped-hold", "1 Hold click")
            step("mapped-release", "1 Release click")
            step("settings-stop", "Stop routing")
            step("configured-page", "Profiles")
            step("save-configured", "Save profile", {"profile_path": configured_file})
            if args.assigned:
                omitted = {"wheel-user", "wheel-tab", "convert", "apply-wheel", "wheel-lab", "converted-wheel",
                           "buttons-user", "buttons-tab", "apply-map", "mapped-lab", "mapped-hold", "mapped-release"}
                steps[:] = [s for s in steps if s["id"] not in omitted]
                evidence["scope"] = "Bounded assigned-process smoke: native curve, per-user held release, profile save and stop cleanup."
        if args.clip_boundary:
            steps.clear()
            prior_clip = {"version": 1, "events": [{"at_micros": 0, "action": {"type": "text", "user": 0, "text": "retain prior valid clip"}}]}
            invalid_clip = {"version": 1, "events": [{"at_micros": 0, "action": {"type": "text", "user": 99, "text": "invalid unknown user"}}]}
            valid_file, invalid_file, retained_file = run / "valid.json", run / "invalid.json", run / "retained.json"
            valid_file.write_text(json.dumps(prior_clip), encoding="utf-8")
            invalid_file.write_text(json.dumps(invalid_clip), encoding="utf-8")
            step("start", "Start routing", wait=1800)
            step("boundary-page", "Macros & replication")
            step("boundary-valid-load", "Load macro", {"macro_path": valid_file})
            step("boundary-invalid-load", "Load macro", {"macro_path": invalid_file})
            step("boundary-save", "Save macro", {"macro_path": retained_file})
            step("boundary-stop", "Stop routing")
            evidence["scope"] = "Six actual controls: rejected unknown-user clip preserves acknowledged GUI cache and live session."
        if args.appearance:
            steps.clear()
            defaults_file, configured_file = run / "appearance-defaults.json", run / "appearance-configured.json"
            step("appearance-defaults-page", "Profiles")
            step("appearance-defaults-save", "Save profile", {"profile_path": defaults_file})
            step("appearance-lab", "Input lab")
            step("start", "Start routing", wait=1800)
            for index in range(2):
                step(f"select-{index}", f"{index+1} Hold click", wait=1400)
                step(f"drop-{index}", f"{index+1} Release click")
            step("appearance-user", "Configure user", {"selected_user": 0})
            step("appearance-theme", "Theme")
            for shape, label in (("arrow", "Arrow cursor"), ("crosshair", "Crosshair cursor"), ("i_beam", "I-beam cursor"), ("ring", "Ring cursor")):
                step("appearance-choice-"+shape, label)
                step("appearance-apply-"+shape, "Apply cursor settings", {"badge": "AB東京🙂123", "label": "true", "size": 28})
            step("appearance-visibility-tab", "Overlay")
            step("appearance-hide", "Hide cursor")
            step("appearance-hide-apply", "Apply cursor settings")
            step("appearance-show", "Show cursor")
            step("appearance-show-apply", "Apply cursor settings")
            step("appearance-lock", "Lock")
            if not args.assigned and not args.appearance_tail:
                for mode, label in (("legacy", "Legacy bounds"), ("desktop", "Desktop bounds"), ("window", "Window bounds"), ("monitor", "Monitor bounds")):
                    step("appearance-bound-choice-"+mode, label, {"monitor_id": monitor["id"]} if mode == "monitor" else None)
                    step("appearance-bound-apply-"+mode, "Apply cursor settings")
            step("appearance-hold-lab", "Input lab")
            step("appearance-held", "Hold all users")
            step("appearance-bound-user", "Configure user", {"selected_user": 0})
            step("appearance-region-lock", "Lock")
            region_fields = {"monitor_id": "none", "monitor_work_area": "false", "region": json.dumps(first_region)}
            step("appearance-region-choice", "Region bounds", region_fields)
            step("appearance-region-apply", "Apply cursor settings", region_fields)
            step("appearance-pause", "Pause")
            step("appearance-paused-apply", "Apply cursor settings", {"region": json.dumps(paused_region)})
            step("appearance-resume", "Resume")
            step("appearance-stop", "Stop routing")
            step("appearance-save-page", "Profiles")
            step("appearance-save", "Save profile", {"profile_path": configured_file})
            evidence["scope"] = "Owned virtual x64/x86 overlays, actual shape/bounds controls, profile defaults and paused bounds resynchronization."
            if args.assigned or args.appearance_tail:
                omitted = {"appearance-choice-"+s for s in ("arrow", "crosshair", "i_beam")} | {"appearance-apply-"+s for s in ("arrow", "crosshair", "i_beam")}
                omitted |= {"appearance-visibility-tab", "appearance-hide", "appearance-hide-apply", "appearance-show", "appearance-show-apply"}
                steps[:] = [s for s in steps if s["id"] not in omitted]
                evidence["scope"] = "Bounded appearance/bounds lifecycle tail: ring hotspot, per-user region apply, paused clamp/resume native alignment, profile save and cleanup."
        (run / "ui.json").write_text(json.dumps(steps, ensure_ascii=False), encoding="utf-8")
        owned.append(subprocess.Popen([str(image), "gui", "--profile", str(run / "profile.json"), "--ui-script", str(run / "ui.json")], cwd=root, creationflags=subprocess.CREATE_NO_WINDOW))
        deadline, last_count, changed = time.monotonic() + 240, 0, time.monotonic()
        def report(index):
            return json.loads((run / f"probe-{index}.json").read_text(encoding="utf-8"))
        def keydelta(current, previous, user, amount):
            assert current["probes"][user]["key_event_count"] == previous["probes"][user]["key_event_count"] + amount, (user, previous["probes"][user]["key_event_count"], current["probes"][user]["key_event_count"], amount)
        while time.monotonic() < deadline:
            assert owned[-1].poll() is None, "GUI exited before acceptance completed"
            log = run / "ui.events.jsonl"
            try:
                events = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]
                if len(events) != last_count:
                    last_count, changed = len(events), time.monotonic()
                if time.monotonic() - changed > 30:
                    progress = (run / "ui.progress.json").read_text(encoding="utf-8")
                    raise AssertionError(f"UI step did not settle within 30s after {last_count}/{len(steps)} clicks: {progress}")
                assert all(e["clicked"] for e in events), events
                if not events or time.monotonic() - changed < .35:
                    time.sleep(.05)
                    continue
                step_id = steps[len(events)-1]["id"]
                if step_id in evidence["samples"]:
                    time.sleep(.05)
                    continue
                if not json.loads((run / "ui.ready.json").read_text(encoding="utf-8")):
                    time.sleep(.05)
                    continue
                state_path = run / "ui.state.json"
                sample = {"probes": [report(i) for i in range(2)], "state": json.loads(state_path.read_text(encoding="utf-8")) if state_path.exists() else [], "button": events[-1]["button"]}
            except (OSError, ValueError):
                time.sleep(.05)
                continue
            samples = evidence["samples"]
            if step_id.startswith("select-"):
                i = int(step_id[-1])
                if sample["state"][i]["pid"] != pids[i] or sample["probes"][i]["buttons"] != 1:
                    continue
            if step_id == "key64":
                keydelta(sample, samples["drop-1"], 0, 2)
                keydelta(sample, samples["drop-1"], 1, 0)
            if step_id == "hold32" and 117 not in sample["probes"][1]["held_keys"]:
                continue
            if step_id == "release32" and 117 in sample["probes"][1]["held_keys"]:
                continue
            expected = {"text64": ["UI64 東京 🦊", ""], "text32": ["UI64 東京 🦊", "UI32 café 🙂"],
                "record-text": ["UI64 東京 🦊R", "UI32 café 🙂"], "play": ["UI64 東京 🦊RRR", "UI32 café 🙂"],
                "replicated-text": ["UI64 東京 🦊RRRZ", "UI32 café 🙂Z"]}
            if step_id in expected and [r["text"] for r in sample["probes"]] != expected[step_id]:
                continue
            if step_id == "save":
                clip = json.loads(recorded.read_text(encoding="utf-8"))
                assert len(clip["events"]) == 3 and [e["action"]["type"] for e in clip["events"]] == ["text", "packet", "packet"], clip
                sample["saved_clip"] = clip
            if step_id == "play":
                # The second loop's text precedes its key events in the recorded clip.
                if sample["probes"][0]["key_event_count"] < samples["load"]["probes"][0]["key_event_count"] + 4:
                    continue
                keydelta(sample, samples["load"], 0, 4)
                keydelta(sample, samples["load"], 1, 0)
            if step_id == "replicated-key":
                keydelta(sample, samples["replication-input-page"], 0, 2)
                keydelta(sample, samples["replication-input-page"], 1, 2)
            if step_id == "play-held" and not (116 in sample["probes"][0]["held_keys"] and sample["probes"][0]["buttons"] == 1):
                continue
            if step_id in ("cancel", "stop"):
                if any(r["held_keys"] or r["buttons"] for r in sample["probes"]):
                    continue
            if step_id == "stop":
                overlays = [u["overlay"]["hwnd"] for u in samples["cancel"]["state"]]
                assert not any(user32.IsWindow(h) for h in overlays), "UI stop left overlays"
            if args.settings:
                if step_id == "save-defaults":
                    saved_profile = json.loads(defaults_file.read_text(encoding="utf-8"))
                    for seat in saved_profile["seats"]:
                        settings = seat["cursor"]
                        assert settings["motion_curve"] == "linear"
                        assert settings["button_mapping"] == ["primary", "secondary", "middle", "x1", "x2"]
                        assert settings["horizontal_wheel"] == "ignore"
                        assert settings["horizontal_wheel_scale"] == 1 and not settings["invert_horizontal_wheel"]
                    sample["saved_profile"] = saved_profile
                if step_id == "settings-held" and not all(r["buttons"] == 1 and 87 in r["held_keys"] for r in sample["probes"]):
                    continue
                if step_id == "invalid-text-held":
                    before_invalid = samples["settings-held"]["probes"]
                    for index, probe in enumerate(sample["probes"]):
                        assert probe["text"] == before_invalid[index]["text"]
                        assert probe["buttons"] == before_invalid[index]["buttons"] and probe["held_keys"] == before_invalid[index]["held_keys"]
                        assert sample["state"][index]["pid"] == pids[index], "invalid GUI text stopped the routing session"
                if step_id == "apply-held":
                    if sample["probes"][0]["held_keys"] or sample["probes"][0]["buttons"]:
                        continue
                    assert sample["probes"][1]["buttons"] == 1 and 87 in sample["probes"][1]["held_keys"], "settings released the other user's input"
                if step_id == "strong-motion":
                    before_motion = samples["motion-lab"]["probes"]
                    delta = sample["probes"][0]["cursor"][0] - before_motion[0]["cursor"][0]
                    if delta == 0:
                        continue
                    assert 40 < delta < 110 and sample["probes"][0]["cursor"][1] == before_motion[0]["cursor"][1]
                    assert sample["probes"][1]["cursor"] == before_motion[1]["cursor"]
                if step_id == "converted-wheel":
                    before_wheel = samples["wheel-lab"]["probes"]
                    if sample["probes"][0]["wheel_events"] < before_wheel[0]["wheel_events"] + 1:
                        continue
                    assert sample["probes"][0]["wheel_events"] == before_wheel[0]["wheel_events"] + 1
                    assert sample["probes"][0]["wheel_delta"] == before_wheel[0]["wheel_delta"] - 240
                    assert sample["probes"][1]["wheel_events"] == before_wheel[1]["wheel_events"]
                if step_id == "apply-wheel" and (run / "ui.png").exists():
                    shutil.copy2(run / "ui.png", run / "wheel-controls.png")
                if step_id == "mapped-hold":
                    if sample["probes"][0]["buttons"] != 8:
                        continue
                    assert sample["probes"][0]["mouse_events"][-1] == {"button": 3, "pressed": True}
                if step_id == "mapped-release":
                    if sample["probes"][0]["buttons"]:
                        continue
                    assert sample["probes"][0]["mouse_events"][-1] == {"button": 3, "pressed": False}
                if step_id == "settings-stop":
                    if any(r["held_keys"] or r["buttons"] for r in sample["probes"]):
                        continue
                    overlays = [u["overlay"]["hwnd"] for u in samples["motion-lab" if args.assigned else "mapped-release"]["state"]]
                    assert not any(user32.IsWindow(h) for h in overlays)
                if step_id == "save-configured":
                    saved_profile = json.loads(configured_file.read_text(encoding="utf-8"))
                    settings = saved_profile["seats"][0]["cursor"]
                    assert settings["motion_curve"] == "strong"
                    if args.assigned:
                        assert settings["button_mapping"] == ["primary", "secondary", "middle", "x1", "x2"]
                        assert settings["horizontal_wheel"] == "ignore"
                    else:
                        assert settings["button_mapping"] == ["x1", "secondary", "middle", None, "x2"]
                        assert settings["horizontal_wheel"] == "convert_to_vertical"
                        assert settings["horizontal_wheel_scale"] == 2 and settings["invert_horizontal_wheel"]
                    assert saved_profile["seats"][1]["cursor"] == samples["save-defaults"]["saved_profile"]["seats"][1]["cursor"]
                    sample["saved_profile"] = saved_profile
            if args.clip_boundary:
                if step_id == "boundary-invalid-load":
                    assert len(sample["state"]) == 2 and all(u["pid"] == 0 for u in sample["state"]), "rejected clip disrupted the scoped session"
                    assert all(not r["held_keys"] and not r["buttons"] and not r["text"] for r in sample["probes"])
                if step_id == "boundary-save":
                    retained = json.loads(retained_file.read_text(encoding="utf-8"))
                    assert retained == prior_clip, (prior_clip, retained)
                    sample["saved_clip"] = retained
                if step_id == "boundary-stop":
                    overlays = [u["overlay"]["hwnd"] for u in samples["boundary-save"]["state"]]
                    assert not any(user32.IsWindow(h) for h in overlays)
            if args.appearance:
                def check_overlay_geometry(index=0, native=True):
                    overlay = sample["state"][index]["overlay"]
                    if native and sample["probes"][index]["cursor"] != [overlay["x"], overlay["y"]]:
                        return False
                    rect = appearance_module.Rect()
                    assert user32.GetWindowRect(overlay["hwnd"], ctypes.byref(rect))
                    assert [rect.left, rect.top] == overlay["render_origin"], (overlay, [rect.left, rect.top])
                    assert [rect.left + overlay["hotspot"][0], rect.top + overlay["hotspot"][1]] == [overlay["x"], overlay["y"]]
                    assert bool(user32.IsWindowVisible(overlay["hwnd"])) == overlay["visible"]
                    sample["native_overlay_rect"] = [rect.left, rect.top, rect.right, rect.bottom]
                    return True
                if step_id == "appearance-defaults-save":
                    saved_profile = json.loads(defaults_file.read_text(encoding="utf-8"))
                    for seat in saved_profile["seats"]:
                        settings = seat["cursor"]
                        assert settings["shape"] == "arrow" and settings["badge"] is None
                        assert settings["bounds_mode"] == "legacy" and settings["monitor_id"] is None and settings["region"] is None
                        assert not settings["monitor_work_area"]
                    sample["saved_profile"] = saved_profile
                if step_id.startswith("appearance-apply-"):
                    shape = step_id.removeprefix("appearance-apply-")
                    overlay = sample["state"][0]["overlay"]
                    if overlay["shape"] != shape or not check_overlay_geometry():
                        continue
                    assert overlay["badge"] == "AB東京🙂123" and overlay["pixel_hash"] != 0 and overlay["painted_pixels"] > 0
                    if shape == "ring" and not args.assigned and not args.appearance_tail:
                        hashes = [samples["appearance-apply-"+s]["state"][0]["overlay"]["pixel_hash"] for s in ("arrow", "crosshair", "i_beam")]
                        assert len(set(hashes + [overlay["pixel_hash"]])) == 4
                if step_id in ("appearance-hide-apply", "appearance-show-apply"):
                    expected_visible = step_id == "appearance-show-apply"
                    overlay = sample["state"][0]["overlay"]
                    if overlay["visible"] != expected_visible or not check_overlay_geometry():
                        continue
                    assert bool(overlay["pixel_hash"]) == expected_visible
                if step_id.startswith("appearance-bound-apply-"):
                    mode = step_id.removeprefix("appearance-bound-apply-")
                    expected = sample["state"][0]["bounds"] if mode == "window" else monitor["bounds"] if mode == "monitor" else displays["desktop"]
                    assert sample["state"][0]["movement_bounds"] == expected
                    if not check_overlay_geometry():
                        continue
                if step_id == "appearance-held" and not all(r["buttons"] == 1 and 87 in r["held_keys"] for r in sample["probes"]):
                    continue
                if step_id == "appearance-region-apply":
                    if sample["state"][0]["movement_bounds"] != first_region or sample["probes"][0]["cursor"] != [first_region["left"], first_region["top"]]:
                        continue
                    if sample["probes"][0]["held_keys"] or sample["probes"][0]["buttons"]:
                        continue
                    assert sample["probes"][1]["buttons"] == 1 and 87 in sample["probes"][1]["held_keys"]
                    if not check_overlay_geometry():
                        continue
                if step_id == "appearance-pause":
                    if any(r["held_keys"] or r["buttons"] for r in sample["probes"]):
                        continue
                    assert sample["state"][0]["overlay"]["pixel_hash"] != samples["appearance-region-apply"]["state"][0]["overlay"]["pixel_hash"], "pause did not update rendered cursor status"
                if step_id == "appearance-paused-apply":
                    overlay = sample["state"][0]["overlay"]
                    assert sample["state"][0]["movement_bounds"] == paused_region
                    assert [overlay["x"], overlay["y"]] == [paused_region["left"], paused_region["top"]]
                    assert sample["probes"][0]["cursor"] == samples["appearance-pause"]["probes"][0]["cursor"]
                    check_overlay_geometry(native=False)
                if step_id == "appearance-resume":
                    if sample["probes"][0]["cursor"] != [paused_region["left"], paused_region["top"]] or not check_overlay_geometry():
                        continue
                    assert sample["state"][0]["overlay"]["pixel_hash"] != samples["appearance-paused-apply"]["state"][0]["overlay"]["pixel_hash"]
                if step_id == "appearance-stop":
                    overlays = [u["overlay"]["hwnd"] for u in samples["appearance-resume"]["state"]]
                    assert not any(user32.IsWindow(h) for h in overlays)
                    assert all(not r["held_keys"] and not r["buttons"] for r in sample["probes"])
                if step_id == "appearance-save":
                    saved_profile = json.loads(configured_file.read_text(encoding="utf-8"))
                    settings = saved_profile["seats"][0]["cursor"]
                    assert settings["shape"] == "ring" and settings["badge"] == "AB東京🙂123" and settings["visible"]
                    assert settings["bounds_mode"] == "region" and settings["region"] == paused_region and settings["monitor_id"] is None
                    assert saved_profile["seats"][1]["cursor"] == samples["appearance-defaults-save"]["saved_profile"]["seats"][1]["cursor"]
                    sample["saved_profile"] = saved_profile
            samples[step_id] = sample
            evidence["clicked_events"] = events
            (run / "evidence.json").write_text(json.dumps(evidence, ensure_ascii=False, indent=2), encoding="utf-8")
            (run / "ui.sampled.json").write_text(str(len(events)), encoding="utf-8")
            print(f"UI accepted {step_id}: {events[-1]['button']}", flush=True)
            if step_id == ("appearance-save" if args.appearance else ("boundary-stop" if args.clip_boundary else ("save-configured" if args.settings else "stop"))):
                assert len(samples) == len(steps)
                time.sleep(.5)
                assert "late-ui" not in report(0)["text"]
                evidence["result"] = "PASS"
                print(f"PASS: actual Ouroboros controls with native x64/x86 evidence. Evidence: {run}", flush=True)
                break
        else:
            progress = (run / "ui.progress.json").read_text(encoding="utf-8")
            raise AssertionError(f"UI gate timeout after {last_count}/{len(steps)}: {progress}")
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
        (run / "evidence.json").write_text(json.dumps(evidence, ensure_ascii=False, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
