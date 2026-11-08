"""Finite Preserve gate: native owned x64/x86 messages, MCP and one macro/edge.

Run only in the exclusive native acceptance slot after the producer's READY.
No hardware input, OS input injection, Cargo, or unrelated application capture.
"""
import argparse
import copy
import hashlib
import importlib.util
from pathlib import Path
import time

_spec = importlib.util.spec_from_file_location("wheel_base", Path(__file__).with_name("test-automation-acceptance.py"))
_base = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_base)
eventually = _base.eventually


class WheelGate(_base.Gate):
    fixture_count = 2
    tool_names = _base._client_module.EXPECTED_TOOLS
    run_prefix = "acceptance-horizontal-wheel"

    def start(self):
        super().start()
        self.evidence["build_sha256"] = {str(p): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in (self.run / "control64.exe", self.run / "probe32.exe")}
        self.defaults = self.state()["user_settings"][0]["settings"]
        schema = next(t["inputSchema"] for t in self.evidence["tools"] if t["name"] == "routing_settings")
        node = schema["properties"]["settings"]
        def resolve(value):
            while "$ref" in value:
                ref = value["$ref"]
                assert ref.startswith("#/"), ref
                value = schema
                for part in ref[2:].split("/"):
                    value = value[part.replace("~1", "/").replace("~0", "~")]
            return value
        policy = resolve(resolve(node)["properties"]["horizontal_wheel"])
        assert set(policy["enum"]) == {"ignore", "convert_to_vertical", "preserve"}
        assert self.defaults["horizontal_wheel"] == "ignore"

    def apply(self, user, **changes):
        value = copy.deepcopy(self.defaults)
        value.update(changes)
        ack = self.client.tool("routing_settings", {"user": user, "settings": value})
        assert next(u["settings"] for u in ack["user_settings"] if u["user"] == user) == value

    @staticmethod
    def protected(state):
        return [{k: u[k] for k in ("pid", "keyboard_pid", "root", "captured", "buttons", "keys", "client")}
                for u in state["users"]]

    def event(self, user, delta, expected, modifiers=0):
        before, state = self.reports(), self.state()
        self.input("wheel", user=user, axis="horizontal", delta=delta)
        after = eventually(lambda: self.report(user), lambda r:
            r["horizontal_wheel_events"] == before[user]["horizontal_wheel_events"] + 1)
        events = [e for e in after["native_events"] if e["kind"] == "horizontal_wheel"]
        actual = events[-1]
        assert actual["delta"] == expected, actual
        assert actual["wheel_screen"] == list(self.points[user]), (actual, self.points[user])
        assert actual["wheel_modifiers"] == modifiers, actual
        assert after["horizontal_wheel_delta"] == before[user]["horizontal_wheel_delta"] + expected
        assert after["wheel_events"] == before[user]["wheel_events"]
        assert after["mouse_event_count"] == before[user]["mouse_event_count"]
        assert after["cursor"] == before[user]["cursor"]
        other = self.report(1-user)
        for field in ("wheel_events", "horizontal_wheel_events", "mouse_event_count", "key_event_count", "cursor"):
            assert other[field] == before[1-user][field], ("other target changed", field)
        assert self.protected(self.state()) == self.protected(state), "wheel acquired root/capture/held input"
        return actual

    def signs(self):
        delivered = []
        for user in range(2):
            # Zero remains an axis-preserving queued message; it does not become a button.
            for scale, invert, delta, expected in ((1, False, 120, 120), (1, False, -120, -120),
                    (.5, False, 120, 60), (2, True, 120, -240), (2, True, -120, 240),
                    (1, False, 0, 0), (8, False, 32767, 32767), (8, False, -32768, -32768)):
                self.apply(user, horizontal_wheel="preserve", horizontal_wheel_scale=scale,
                           invert_horizontal_wheel=invert)
                delivered.append(self.event(user, delta, expected))
        self.evidence["signed_samples"] = delivered
        self.snapshot("native-preserve-sign-scale-invert-zero-saturation-isolation")

    def modifiers(self):
        self.apply(0, horizontal_wheel="preserve")
        self.key(0, "leftctrl", True)
        self.key(0, "leftshift", True)
        self.input("button", user=0, button="back", pressed=True)
        self.key(1, "f6", True)
        eventually(self.reports, lambda rs: {160, 162} <= set(rs[0]["held_keys"])
            and rs[0]["buttons"] == 8 and 117 in rs[1]["held_keys"])
        self.event(0, -120, -120, modifiers=0x2c)
        self.snapshot("virtual-modifiers-and-existing-capture-holds-preserved")
        self.input("button", user=0, button="back", pressed=False)
        self.key(0, "leftctrl", False)
        self.key(0, "leftshift", False)
        self.key(1, "f6", False)
        self.released()

    def compatibility(self):
        self.apply(0)
        before = self.report(0)
        self.input("wheel", user=0, axis="horizontal", delta=120)
        time.sleep(.2)
        assert self.report(0)["horizontal_wheel_events"] == before["horizontal_wheel_events"]
        assert self.report(0)["wheel_events"] == before["wheel_events"]
        self.apply(0, horizontal_wheel="convert_to_vertical", horizontal_wheel_scale=.5,
                   invert_horizontal_wheel=True, wheel_scale=2)
        self.input("wheel", user=0, axis="horizontal", delta=120)
        after = eventually(lambda: self.report(0), lambda r: r["wheel_events"] == before["wheel_events"]+1)
        assert after["wheel_delta"] == before["wheel_delta"]-60
        self.apply(0, horizontal_wheel="preserve", wheel_scale=.5, invert_wheel=True)
        self.input("wheel", user=0, axis="vertical", delta=120)
        vertical = eventually(lambda: self.report(0), lambda r: r["wheel_events"] == before["wheel_events"]+2)
        assert vertical["wheel_delta"] == before["wheel_delta"]-120
        assert vertical["horizontal_wheel_events"] == before["horizontal_wheel_events"]
        self.snapshot("small-ignore-convert-and-vertical-compatibility-baseline")

    def macro_edge(self):
        self.apply(0, horizontal_wheel="preserve", horizontal_wheel_scale=.5)
        self.apply(1, horizontal_wheel="preserve", horizontal_wheel_scale=2, invert_horizontal_wheel=True)
        self.client.tool("routing_replication", {"edges": [{"source": 0, "target": 1,
            "mouse": True, "keyboard": False, "text": False}]})
        before = self.reports()
        self.client.tool("automation_record", {"recording": True})
        self.input("wheel", user=0, axis="horizontal", delta=120)
        clip = self.client.tool("automation_record", {"recording": False})["automation"]["clip"]
        assert len(clip["events"]) == 1 and clip["events"][0]["action"]["packet"]["flags"] == 0x800
        assert clip["events"][0]["action"]["packet"]["data"] == 120
        for i, delta in enumerate((60, -240)):
            result = eventually(lambda i=i: self.report(i), lambda r, i=i:
                r["horizontal_wheel_events"] == before[i]["horizontal_wheel_events"]+1)
            assert result["horizontal_wheel_delta"] == before[i]["horizontal_wheel_delta"]+delta
        self.client.tool("automation_clip", {"action": "load", "clip": clip})
        self.client.tool("automation_play", {"loops": 1, "speed": 1})
        eventually(self.state, lambda s: s["automation"]["playback"]["state"] == "completed")
        for i, delta in enumerate((60, -240)):
            result = eventually(lambda i=i: self.report(i), lambda r, i=i:
                r["horizontal_wheel_events"] == before[i]["horizontal_wheel_events"]+2)
            assert result["horizontal_wheel_delta"] == before[i]["horizontal_wheel_delta"]+2*delta
            assert result["wheel_events"] == before[i]["wheel_events"]
        self.released()
        self.snapshot("one-recorded-horizontal-packet-replayed-and-replicated-once-per-destination")
        self.client.tool("routing_replication", {"edges": []})

    def stop(self):
        hwnds = [u["overlay"]["hwnd"] for u in self.state()["users"]]
        self.client.tool("routing_stop")
        self.released()
        eventually(lambda: [_base.user32.IsWindow(h) for h in hwnds], lambda values: not any(values))
        self.snapshot("normal-stop-native-release-and-overlay-destruction")

    def switched(self):
        self.apply(1, horizontal_wheel="preserve")
        self.client.tool("routing_mode", {"mode": "switched", "multi_keyboard": True})
        self.input("button", user=0, button="primary", pressed=True)
        eventually(self.state, lambda s: s["users"][0]["root"] and s["users"][0]["captured"])
        eventually(lambda: self.report(0), lambda r: r["buttons"] == 1)
        before = self.protected(self.state())
        assert before[0]["root"] and before[0]["captured"]
        assert before[0]["keyboard_pid"] == self.probes[0].pid
        assert before[1]["keyboard_pid"] == self.probes[1].pid
        self.event(1, 120, 120)
        self.event(1, -120, -120)
        assert self.report(0)["buttons"] == 1
        assert self.protected(self.state()) == before
        self.snapshot("switched-other-seat-wheel-preserves-click-root-capture-and-keyboard-selections")
        self.input("button", user=0, button="primary", pressed=False)
        self.released()
        eventually(self.state, lambda s: not any(u["root"] or u["captured"] for u in s["users"]))


def main():
    parser = argparse.ArgumentParser()
    for name in ("executable", "x86-executable", "runtime"):
        parser.add_argument("--"+name, type=Path, required=True)
    parser.add_argument("--switched-only", action="store_true", help="Two-wheel click-arbitration supplement only")
    args = parser.parse_args()
    if args.switched_only:
        WheelGate.run_prefix = "acceptance-horizontal-wheel-switched"
    gate = WheelGate(args)
    try:
        gate.start()
        if args.switched_only:
            gate.switched()
        else:
            gate.signs()
            gate.modifiers()
            gate.compatibility()
            gate.macro_edge()
        gate.stop()
        gate.evidence["result"] = "PASS"
    except Exception as error:
        gate.evidence.update(result="NOT ACCEPTED", failure=repr(error))
        raise
    finally:
        gate.close()
        print(f"{gate.evidence.get('result', 'NOT ACCEPTED')}: {gate.run}", flush=True)


if __name__ == "__main__":
    main()
