"""One edited-clip boundary rejection while native input remains held."""
import argparse
import importlib.util
from pathlib import Path

_spec = importlib.util.spec_from_file_location("settings_gate", Path(__file__).with_name("test-settings-acceptance.py"))
_module = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    args = parser.parse_args()
    class BoundaryGate(_module.SettingsGate):
        run_prefix = "acceptance-settings-boundary"
    gate = BoundaryGate(args)
    try:
        gate.start()
        valid = {"version": 1, "events": [{"at_micros": 0, "action": {"type": "text", "user": 0, "text": "prior-clip"}}]}
        gate.client.tool("automation_clip", {"action": "load", "clip": valid})
        gate.key(0, "f5", True)
        gate.input("button", user=0, button="back", pressed=True)
        _module.eventually(lambda: gate.report(0), lambda r: 116 in r["held_keys"] and r["buttons"] == 8)
        before = gate.reports()
        invalid = {"version": 1, "events": [{"at_micros": 0, "action": {"type": "packet", "user": 0,
            "packet": {"device": 0xF1000002, "kind": 0, "flags": 5, "x": 0, "y": 0, "data": 0}}}]}
        gate.client.tool("automation_clip", {"action": "load", "clip": invalid}, error=True)
        inspected = gate.client.tool("automation_clip", {"action": "inspect"})
        assert inspected["automation"]["clip"] == valid
        after = gate.reports()
        for previous, actual in zip(before, after):
            for field in ("buttons", "held_keys", "mouse_event_count", "key_event_count", "text"):
                assert actual[field] == previous[field], (field, previous, actual)
        gate.snapshot("combined-edge-edited-clip-rejected-preserves-native-holds-and-prior-clip")
        gate.input("release_all")
        gate.released()
        gate.client.tool("routing_stop")
        gate.evidence["result"] = "PASS"
        print(f"PASS: combined-edge boundary rejection. Evidence: {gate.run}", flush=True)
    except Exception as error:
        gate.evidence["result"] = "NOT ACCEPTED"
        gate.evidence["failure"] = repr(error)
        print(f"NOT ACCEPTED: {error!a}. Evidence: {gate.run}", flush=True)
        raise
    finally:
        gate.close()


if __name__ == "__main__":
    main()
