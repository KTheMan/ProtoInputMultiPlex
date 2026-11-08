"""Focused genuine overflow-caption view; fresh own x64/x86 targets only."""
import argparse
import copy
import importlib.util
from pathlib import Path
from PIL import Image

_spec = importlib.util.spec_from_file_location("caption_helpers", Path(__file__).with_name("test-replication-graph-ui.py"))
_ui = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_ui)
s = _ui._native._studio


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--profile", type=Path, required=True)
    parser.add_argument("--ready", action="store_true")
    args = parser.parse_args()
    assert args.ready
    gate = _ui.UiGate(args, "advanced-caption", count=2)
    gate.fixtures()
    profile = copy.deepcopy(_ui.read_json(args.profile))
    profile["desktop"]["allowed_pids"] = gate.pids
    for i, seat in enumerate(profile["seats"]):
        seat["desktop_start"] = gate.points[i]
    _ui.put(gate.run / "overflow.json", s.clip(*[s.event(0, s.button(0, i % 2 == 0, code=3)) for i in range(4096)], s.event(0, s.button(0, True, code=4))))
    step = _ui.step
    steps = [step("input-page", "Input lab"), step("start", "Start routing"), step("select-a", "1 Hold click"), step("drop-a", "1 Release click"), step("select-b", "2 Hold click"), step("drop-b", "2 Release click"),
             step("studio-page", "Macro Studio"), step("studio-select", "Inspect user macro", {"studio_user": 0}), step("load-overflow", "Load user clip JSON", {"studio_file_path": gate.run / "overflow.json"}), step("play-overflow", "Play user macro", {"studio_loops": 1, "studio_repeat": "false", "studio_keep_awake": "false"}),
             step("overflow-page", "Replication Graph"), step("caption-cancel", "Cancel pending replication"),
             # Keep the next real target at the top while the first Cancel
             # awaits its fresh PNG sample; observing Apply would scroll away.
             step("caption-hold", "Cancel pending replication"), step("apply-resume", "Apply replication graph"), step("stop", "Stop routing")]
    baseline = None
    def verify(name, sample):
        nonlocal baseline
        if name == "load-overflow":
            baseline = [r["mouse_event_count"] for r in sample["probes"]]
        if name in ("overflow-page", "caption-cancel"):
            queue = sample["graph"]["replication"]
            if not queue["suspended"] or sample["probes"][0]["mouse_event_count"] != baseline[0] + 4096:
                return False
            assert queue["error"] and not queue["pending_events"] and not queue["pending_bytes"]
            assert sample["probes"][1]["mouse_event_count"] == baseline[1]
            if name == "caption-cancel":
                png = gate.run / "ui.png"
                if not png.exists() or png.stat().st_mtime < (gate.run / "ui.events.jsonl").stat().st_mtime:
                    return False
                with Image.open(png) as image:
                    image.load()
                    image.save(gate.run / "overflow-caption.png")
                sample["owned_caption_screenshot"] = str(gate.run / "overflow-caption.png")
        if name == "apply-resume":
            if sample["graph"]["pending"] or sample["graph"]["replication"]["suspended"]:
                return False
            assert not sample["graph"]["replication"]["error"] and sample["graph"]["applied"] == profile["replication"]
        if name == "stop":
            if any(r["buttons"] or r["held_keys"] for r in sample["probes"]):
                return False
            hwnds = [u["overlay"]["hwnd"] for u in gate.evidence["samples"]["load-overflow"]["state"]]
            if any(_ui.user32.IsWindow(h) for h in hwnds):
                return False
        return True
    gate.execute(profile, steps, verify)


if __name__ == "__main__":
    main()
