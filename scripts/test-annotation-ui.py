"""Bounded real Ouroboros annotation controls, app-local script clicks only."""
import argparse
import ctypes
from ctypes import wintypes
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import time
from PIL import Image

_spec = importlib.util.spec_from_file_location("annotation_gate", Path(__file__).with_name("test-annotation-acceptance.py"))
_module = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)
user32, eventually = _module.user32, _module.eventually
Client = _module._module._module._module.Client


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--assigned-smoke", action="store_true")
    args = parser.parse_args()
    run = args.runtime.resolve() / time.strftime(("acceptance-assigned-annotation-ui" if args.assigned_smoke else "acceptance-annotation-ui")+"-%Y%m%d-%H%M%S")
    run.mkdir(parents=True)
    owned, client = [], None
    evidence = {"result": "NOT ACCEPTED", "samples": {}, "scope": "Actual annotation controls, native virtual x64/x86 messages, independent PNG decoding, defaults/profile, genuine worker export failure, pause/stop cleanup."}
    try:
        images = [run/"control64.exe", run/"probe32.exe"]
        for image, source in zip(images, (args.executable, args.x86_executable)):
            shutil.copy2(source, image)
        for index, image in enumerate(images):
            r, g, b = _module.AnnotationGate.colors[index]
            owned.append(subprocess.Popen([str(image), "probe", "--automation", "--capture-pattern", str(r<<16|g<<8|b), "--report", str(run/f"probe-{index}.json")], creationflags=subprocess.CREATE_NO_WINDOW))
        client = Client(images[0], args.runtime.resolve(), run/"discovery.stderr.log")
        assert {t["name"] for t in client.rpc("tools/list", {})["tools"]} == _module.AnnotationGate.tool_names
        pids = [p.pid for p in owned]
        inventory = eventually(lambda: client.tool("routing_windows"), lambda v: all(any(w["pid"] == p and w["title"].startswith("ProtoInputMultiPlex Automation Test") for w in v["windows"]) for p in pids), 60)
        windows = [next(w for w in inventory["windows"] if w["pid"] == p and w["title"].startswith("ProtoInputMultiPlex Automation Test")) for p in pids]
        for i, window in enumerate(windows):
            assert user32.SetWindowPos(window["hwnd"], wintypes.HWND(-1), 30+i*460, 40, 420, 360, 0x10)
        inventory = client.tool("routing_windows")
        windows = [next(w for w in inventory["windows"] if w["pid"] == p and w["title"].startswith("ProtoInputMultiPlex Automation Test")) for p in pids]
        points = [[w["client"]["left"]+120, w["client"]["top"]+100] for w in windows]
        if args.assigned_smoke:
            points = [[(w["client"]["left"]+w["client"]["right"])//2,(w["client"]["top"]+w["client"]["bottom"])//2] for w in windows]
        desktop = client.tool("routing_displays")["desktop"]
        client.close()
        client = None
        profile = {"version": 1, "concurrent": True, "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": pids},
                   "seats": [{"name": f"Annotation UI{i+1}", "mouse": f"virtual://mouse/{i+1}", "keyboard": f"virtual://keyboard/{i+1}", "desktop_start": points[i], "cursor": {"lock_window": False}} for i in range(2)]}
        if args.assigned_smoke:
            profile["desktop"] = None
            for i, seat in enumerate(profile["seats"]):
                seat["target_pid"] = pids[i]
        (run/"profile.json").write_text(json.dumps(profile), encoding="utf-8")
        steps = []
        def step(name, button, fields=None, wait=450):
            steps.append({"id": name, "button": button, "wait_ms": wait, "wait_for_sample": True, "fields": {k: str(v) for k, v in (fields or {}).items()}})
        step("default-page", "Profiles")
        step("default-save", "Save profile", {"profile_path": run/"defaults.json"})
        step("input-start-page", "Input lab")
        step("start", "Start routing", wait=1800)
        for i in range(2):
            step(f"select-{i}", f"{i+1} Hold click", wait=1200)
            step(f"release-{i}", f"{i+1} Release click")
        step("annotation-page", "Annotation")
        step("brush", "Annotation Brush")
        for label in ("Secondary", "Middle", "Back", "Forward", "Primary"):
            step("activator-"+label.lower(), "Activator "+label)
        step("apply-0", "Apply annotation settings", {"annotation_width": 8, "annotation_red": 220, "annotation_green": 20, "annotation_blue": 20, "annotation_alpha": 255, "annotation_clear_key": "F8"})
        step("user-1", "Select annotation user 2")
        step("brush-1", "Annotation Brush")
        step("apply-1", "Apply annotation settings", {"annotation_width": 8, "annotation_red": 20, "annotation_green": 220, "annotation_blue": 20, "annotation_alpha": 128})
        step("drawing-page", "Input lab")
        for i in range(2):
            step(f"ink-down-{i}", f"{i+1} Hold click")
            step(f"ink-move-{i}", f"{i+1} Right")
            step(f"ink-up-{i}", f"{i+1} Release click")
        step("other-held", "Hold key", {"selected_user": 1, "key_name": "f5"})
        step("exports-page", "Annotation")
        step("export-kind", "Export transparent ink PNG")
        step("ink-save", "Save annotation PNG", {"annotation_export_path": run/"both.png"})
        step("owner-0", "Select annotation user 1")
        step("eraser", "Annotation Eraser")
        step("eraser-apply", "Apply annotation settings", {"annotation_width": 12})
        step("erase-page", "Input lab")
        step("erase-down", "1 Hold click")
        step("erase-move", "1 Left")
        step("erase-up", "1 Release click")
        step("clear-page", "Annotation")
        step("clear-mine", "Clear selected user's ink")
        step("mine-save", "Save annotation PNG", {"annotation_export_path": run/"mine-cleared.png"})
        step("bad-export", "Save annotation PNG", {"annotation_export_path": run/"absent"/"bad.png"})
        step("composition-kind", "Export selected target composition PNG")
        step("composition-save", "Save annotation PNG", {"annotation_export_path": run/"composition.png"})
        step("clear-all", "Clear all users' ink")
        step("lifecycle-brush", "Annotation Brush")
        step("lifecycle-apply", "Apply annotation settings")
        step("lifecycle-input", "Input lab")
        step("lifecycle-down", "1 Hold click")
        step("lifecycle-pause", "Pause")
        step("lifecycle-resume", "Resume")
        step("lifecycle-motion", "1 Right")
        step("lifecycle-up", "1 Release click")
        step("lifecycle-page", "Annotation")
        step("off", "Annotation Off")
        step("off-apply", "Apply annotation settings", {"annotation_clear_key": "none"})
        step("stop", "Stop routing")
        step("profile-page", "Profiles")
        step("profile-save", "Save profile", {"profile_path": run/"configured.json"})
        step("profile-load", "Load profile", {"profile_path": run/"configured.json"})
        step("profile-roundtrip", "Save profile", {"profile_path": run/"roundtrip.json"})
        if args.assigned_smoke:
            selected = {"input-start-page","start","annotation-page","brush","apply-0","user-1","brush-1","apply-1","drawing-page",
                        "ink-down-0","ink-move-0","ink-up-0","ink-down-1","ink-move-1","ink-up-1","exports-page","export-kind","ink-save",
                        "owner-0","composition-kind","composition-save","clear-all","off","off-apply","stop"}
            steps[:] = [s for s in steps if s["id"] in selected]
            evidence["scope"] = "Bounded assigned-root annotation smoke: per-user settings/native consumed edges/RGBA, exact assigned root client PrintWindow composition, clear and Stop cleanup."
        (run/"ui.json").write_text(json.dumps(steps), encoding="utf-8")
        owned.append(subprocess.Popen([str(images[0]), "gui", "--profile", str(run/"profile.json"), "--ui-script", str(run/"ui.json")], cwd=Path(__file__).resolve().parent.parent, creationflags=subprocess.CREATE_NO_WINDOW))
        deadline, last_count, changed = time.monotonic()+300, 0, time.monotonic()
        while time.monotonic() < deadline:
            assert owned[-1].poll() is None, "GUI exited before gate completed"
            try:
                events = [json.loads(line) for line in (run/"ui.events.jsonl").read_text(encoding="utf-8").splitlines()]
                if len(events) != last_count:
                    last_count, changed = len(events), time.monotonic()
                assert time.monotonic()-changed < 30, f"UI failed to settle after {last_count}/{len(steps)} controls"
                if not events or not json.loads((run/"ui.ready.json").read_text(encoding="utf-8")):
                    time.sleep(.05)
                    continue
                name = steps[len(events)-1]["id"]
                if name in evidence["samples"]:
                    time.sleep(.05)
                    continue
                assert all(e["clicked"] for e in events)
                probes = [json.loads((run/f"probe-{i}.json").read_text(encoding="utf-8")) for i in range(2)]
                state = json.loads((run/"ui.state.json").read_text(encoding="utf-8")) if (run/"ui.state.json").exists() else []
                annotation = json.loads((run/"ui.annotation.json").read_text(encoding="utf-8")) if (run/"ui.annotation.json").exists() else None
                acks = [json.loads(line) for line in (run/"ui.annotation.acks.jsonl").read_text(encoding="utf-8").splitlines()] if (run/"ui.annotation.acks.jsonl").exists() else []
                sample = {"probes": probes, "state": state, "annotation": annotation, "acknowledgements": acks, "button": events[-1]["button"]}
            except (OSError, ValueError):
                time.sleep(.05)
                continue
            samples = evidence["samples"]
            if name.startswith("select-"):
                i = int(name[-1])
                if len(state) != 2 or state[i]["pid"] != pids[i] or probes[i]["buttons"] != 1:
                    continue
            if name in ("apply-0", "apply-1", "eraser-apply", "off-apply", "lifecycle-apply"):
                i = 1 if name == "apply-1" else 0
                wanted = "eraser" if name == "eraser-apply" else ("off" if name == "off-apply" else "brush")
                if not annotation or annotation["users"][i]["settings"]["tool"] != wanted:
                    continue
                settings = annotation["users"][i]["settings"]
                if name == "apply-0":
                    assert settings["color"] == [220,20,20] and settings["alpha"] == 255 and settings["clear_key"] == 119 and settings["width"] == 8
                if name == "apply-1":
                    assert settings["color"] == [20,220,20] and settings["alpha"] == 128
            if name.startswith("ink-down-"):
                i = int(name[-1])
                if not annotation or not annotation["users"][i]["open"]:
                    continue
                assert probes[i]["mouse_event_count"] == samples["drawing-page"]["probes"][i]["mouse_event_count"]
            if name.startswith("ink-up-"):
                i = int(name[-1])
                if annotation["users"][i]["open"]:
                    continue
                assert probes[i]["mouse_event_count"] == samples["drawing-page"]["probes"][i]["mouse_event_count"]
            if name in ("ink-save", "mine-save", "composition-save"):
                filename = {"ink-save":"both.png", "mine-save":"mine-cleared.png", "composition-save":"composition.png"}[name]
                if not (run/filename).exists():
                    continue
                with Image.open(run/filename) as image:
                    assert image.mode == "RGBA"
                    if name == "composition-save":
                        expected_hwnd = windows[0]["hwnd"] if args.assigned_smoke else probes[0]["capture_hwnd"]
                        assert acks[-1]["export"]["hwnd"] == expected_hwnd and acks[-1]["export"]["pid"] == pids[0]
                        if args.assigned_smoke:
                            assert image.size == (windows[0]["client"]["right"]-windows[0]["client"]["left"],windows[0]["client"]["bottom"]-windows[0]["client"]["top"])
                            assert image.getpixel((28,38)) == (255,255,255,255)
                            assert image.getpixel((60,70)) == (*_module.AnnotationGate.colors[0],255)
                        else:
                            assert image.getpixel((8,8)) == (255,255,255,255)
                            assert image.getpixel((40,40)) == (*_module.AnnotationGate.colors[0],255)
                    else:
                        def pixel(i):
                            return image.getpixel((points[i][0]+10-desktop["left"],points[i][1]-desktop["top"]))
                        if name == "ink-save":
                            assert pixel(0) == (220,20,20,255), pixel(0)
                        else:
                            assert pixel(0)[3] == 0
                        assert pixel(1) == (20,220,20,128), pixel(1)
                    sample["png"] = {"path": str(run/filename), "size": list(image.size)}
            if name == "bad-export":
                if not acks or not acks[-1]["error"]:
                    continue
                assert acks[-1]["users"] == samples["mine-save"]["annotation"]["users"]
                assert acks[-1]["revision"] == samples["mine-save"]["annotation"]["revision"]
                assert not (run/"absent"/"bad.png").exists()
                assert len(state) == 2 and all(state[i]["pid"] == pids[i] for i in range(2))
                assert 116 in probes[1]["held_keys"], "failed export released unrelated held key"
            if name == "other-held" and 116 not in probes[1]["held_keys"]:
                continue
            if name == "lifecycle-down" and not annotation["users"][0]["open"]:
                continue
            if name == "lifecycle-pause" and annotation["users"][0]["open"]:
                continue
            if name == "lifecycle-motion":
                assert annotation["users"][0]["segments"] == samples["lifecycle-pause"]["annotation"]["users"][0]["segments"]
            if name == "clear-all":
                if any(u["segments"] for u in annotation["users"]):
                    continue
            if name == "stop":
                handles = [u["overlay"]["hwnd"] for u in samples["off-apply"]["state"]]
                handles.append(samples["ink-save"]["annotation"]["overlay"]["hwnd"])
                if any(user32.IsWindow(h) for h in handles) or any(p["held_keys"] or p["buttons"] for p in probes):
                    continue
            if name in ("default-save", "profile-save", "profile-roundtrip"):
                filename = {"default-save":"defaults.json", "profile-save":"configured.json", "profile-roundtrip":"roundtrip.json"}[name]
                saved = json.loads((run/filename).read_text(encoding="utf-8"))
                sample["profile"] = saved
                if name == "default-save":
                    assert all(seat["annotation"] == {"tool":"off","activator":"primary","color":[96,165,250],"alpha":255,"width":6,"clear_key":None,"clear_all":False} for seat in saved["seats"])
                else:
                    assert saved["seats"][0]["annotation"]["tool"] == "off" and saved["seats"][0]["annotation"]["clear_key"] is None
                    assert saved["seats"][1]["annotation"]["tool"] == "brush" and saved["seats"][1]["annotation"]["alpha"] == 128
                    if name == "profile-roundtrip":
                        assert saved == samples["profile-save"]["profile"]
            samples[name] = sample
            evidence["clicked_events"] = events
            (run/"evidence.json").write_text(json.dumps(evidence, indent=2), encoding="utf-8")
            (run/"ui.sampled.json").write_text(str(len(events)), encoding="utf-8")
            print(f"UI accepted {name}: {sample['button']}", flush=True)
            if name == ("stop" if args.assigned_smoke else "profile-roundtrip"):
                assert len(samples) == len(steps)
                evidence["result"] = "PASS"
                print(f"PASS: {len(steps)} actual annotation controls. Evidence: {run}", flush=True)
                break
        else:
            raise AssertionError("annotation UI finite deadline exhausted")
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
        (run/"evidence.json").write_text(json.dumps(evidence, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
