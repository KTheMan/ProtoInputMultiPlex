"""Independent settings gate. Virtual-only x64/x86 native fixtures; bounded checks."""
import argparse
import copy
import importlib.util
import json
from pathlib import Path
import time

_spec = importlib.util.spec_from_file_location("automation_gate", Path(__file__).with_name("test-automation-acceptance.py"))
_module = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)
eventually = _module.eventually


class SettingsGate(_module.Gate):
    fixture_count = 2
    tool_names = _module.TOOLS | {"routing_settings"}
    run_prefix = "acceptance-settings"

    def discover(self):
        schema = next(t for t in self.evidence["tools"] if t["name"] == "routing_settings")["inputSchema"]
        def resolve(node):
            while "$ref" in node:
                ref = node["$ref"]
                assert ref.startswith("#/"), ref
                node = schema
                for part in ref[2:].split("/"):
                    node = node[part.replace("~1", "/").replace("~0", "~")]
            return node
        settings_schema = resolve(schema["properties"]["settings"])
        self.defaults = self.state()["user_settings"][0]["settings"]
        assert settings_schema.get("type") == "object", "settings fields undiscoverable"
        assert set(self.defaults) <= set(settings_schema["properties"]), settings_schema
        assert set(resolve(settings_schema["properties"]["motion_curve"])["enum"]) == {"linear", "gentle", "strong"}
        assert set(resolve(settings_schema["properties"]["horizontal_wheel"])["enum"]) == {"ignore", "convert_to_vertical"}
        assert self.defaults["motion_curve"] == "linear"
        assert self.defaults["button_mapping"] == ["primary", "secondary", "middle", "x1", "x2"]
        assert self.defaults["horizontal_wheel"] == "ignore"
        self.snapshot("full-settings-schema-and-default-readback")

    def apply(self, user, **changes):
        value = copy.deepcopy(self.defaults)
        value.update(changes)
        result = self.client.tool("routing_settings", {"user": user, "settings": value})
        acknowledged = next(s["settings"] for s in result["user_settings"] if s["user"] == user)
        assert acknowledged == value, (value, acknowledged)
        return value

    def anchored(self, user):
        x, y = self.points[user]
        self.input("move_to", user=user, x=x, y=y)
        eventually(lambda: self.report(user), lambda r: r["cursor"] == [x, y])

    def motion(self):
        for user in range(2):
            displacements = {}
            for curve in ("linear", "gentle", "strong"):
                self.apply(user, motion_curve=curve)
                self.anchored(user)
                other = self.report(1-user)["cursor"]
                x, y = self.points[user]
                self.input("motion", user=user, dx=2, dy=0)
                eventually(lambda: self.report(user), lambda r: r["cursor"] == [x+2, y])
                self.anchored(user)
                self.input("motion", user=user, dx=20, dy=0)
                sample = eventually(lambda: self.report(user), lambda r: r["cursor"][0] > x)
                displacements[curve] = sample["cursor"][0] - x
                assert sample["cursor"][1] == y
                assert self.report(1-user)["cursor"] == other, "curve motion leaked"
            assert displacements["linear"] == 20
            assert 20 < displacements["gentle"] < 35
            assert displacements["gentle"] < displacements["strong"] < 55
            self.evidence["checks"][f"curve-order-{user}"] = displacements
            self.apply(user, motion_curve="strong", speed_x=2, speed_y=.5, invert_x=True)
            self.anchored(user)
            x, y = self.points[user]
            self.input("motion", user=user, dx=20, dy=20)
            sample = eventually(lambda: self.report(user), lambda r: r["cursor"][0] < x and r["cursor"][1] > y)
            assert 40 < x-sample["cursor"][0] < 110
            assert 10 < sample["cursor"][1]-y < 28
            self.apply(user)
            self.anchored(user)
        self.snapshot("native-curves-speed-axis-inversion-and-isolation")

    def edge(self, user, source, target):
        names = ["primary", "secondary", "middle", "back", "forward"]
        before = self.report(user)
        other = self.report(1-user)["mouse_event_count"]
        self.input("button", user=user, button=names[source], pressed=True)
        held = eventually(lambda: self.report(user), lambda r: r["buttons"] == 1 << target and r["mouse_event_count"] == before["mouse_event_count"] + 1)
        self.input("button", user=user, button=names[source], pressed=False)
        released = eventually(lambda: self.report(user), lambda r: r["buttons"] == 0 and r["mouse_event_count"] == before["mouse_event_count"] + 2)
        assert released["mouse_events"][-2:] == [{"button": target, "pressed": True}, {"button": target, "pressed": False}], released
        assert self.report(1-user)["mouse_event_count"] == other, "button edge leaked"
        return held, released

    def buttons(self):
        for user in range(2):
            for source in range(5):
                self.edge(user, source, source)
            self.apply(user, button_mapping=["x1", "x2", "middle", "secondary", "primary"])
            for source, target in enumerate([3, 4, 2, 1, 0]):
                self.edge(user, source, target)
            self.apply(user, button_mapping=[None, "secondary", "middle", "x1", "x2"])
            before = self.report(user)
            self.input("button", user=user, button="primary", pressed=True)
            self.input("button", user=user, button="primary", pressed=False)
            time.sleep(.25)
            after = self.report(user)
            assert after["mouse_event_count"] == before["mouse_event_count"] and after["buttons"] == 0
            self.apply(user, button_mapping=["x1", "x2", "middle", "secondary", "primary"], swap_buttons=True)
            self.edge(user, 4, 1)  # primary output becomes secondary after the legacy swap.
            self.edge(user, 3, 0)
            self.apply(user)
        self.snapshot("five-native-buttons-mapping-disabled-and-legacy-swap")
        self.input("button", user=0, button="back", pressed=True)
        self.key(0, "f5", True)
        eventually(lambda: self.report(0), lambda r: r["buttons"] == 8 and 116 in r["held_keys"])
        self.apply(0, button_mapping=["x1", "x2", "middle", "secondary", "primary"])
        self.released()
        self.edge(0, 0, 3)
        self.snapshot("live-settings-release-previous-native-holds-before-remap")
        self.apply(0)

    def wheels(self):
        for user in range(2):
            baseline = self.report(user)
            other = self.report(1-user)
            self.input("wheel", user=user, axis="horizontal", delta=120)
            time.sleep(.25)
            ignored = self.report(user)
            for field in ("wheel_events", "wheel_delta", "horizontal_wheel_events", "horizontal_wheel_delta"):
                assert ignored[field] == baseline[field], (field, baseline, ignored)
            self.apply(user, horizontal_wheel="convert_to_vertical", horizontal_wheel_scale=2, invert_horizontal_wheel=True,
                       wheel_scale=.5, invert_wheel=False)
            self.input("wheel", user=user, axis="horizontal", delta=120)
            first = eventually(lambda: self.report(user), lambda r: r["wheel_events"] == baseline["wheel_events"] + 1)
            assert first["wheel_delta"] == baseline["wheel_delta"] - 240
            self.input("wheel", user=user, axis="horizontal", delta=-120)
            second = eventually(lambda: self.report(user), lambda r: r["wheel_events"] == baseline["wheel_events"] + 2)
            assert second["wheel_delta"] == baseline["wheel_delta"]
            self.input("wheel", user=user, axis="vertical", delta=120)
            third = eventually(lambda: self.report(user), lambda r: r["wheel_events"] == baseline["wheel_events"] + 3)
            assert third["wheel_delta"] == baseline["wheel_delta"] + 60
            assert third["horizontal_wheel_events"] == baseline["horizontal_wheel_events"]
            for field in ("wheel_events", "wheel_delta", "horizontal_wheel_events", "horizontal_wheel_delta"):
                assert self.report(1-user)[field] == other[field], "wheel leaked"
            self.apply(user)
        self.snapshot("ignore-and-converted-horizontal-scroll-native-sign-scale-inversion")

    def rejection_cleanup(self):
        accepted = self.state()["user_settings"]
        for change in ({"speed_x": 0}, {"horizontal_wheel_scale": 9}, {"motion_curve": "unknown"},
                       {"button_mapping": ["primary", "primary", None, None, None]}, {"unknown_field": True}):
            invalid = copy.deepcopy(self.defaults)
            invalid.update(change)
            self.client.tool("routing_settings", {"user": 0, "settings": invalid}, error=True)
            assert self.state()["user_settings"] == accepted
        self.client.tool("routing_settings", {"user": 99, "settings": self.defaults}, error=True)
        self.snapshot("invalid-settings-rejected-without-live-mutation")
        before = self.state()
        stopped = self.client.tool("routing_stop")
        assert not stopped["running"] and not stopped["users"]
        self.released()
        eventually(lambda: [bool(_module.user32.IsWindow(u["overlay"]["hwnd"])) for u in before["users"]], lambda live: not any(live))
        self.snapshot("settings-normal-stop-release-and-overlay-cleanup")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    gate = SettingsGate(parser.parse_args())
    try:
        gate.start()
        gate.discover()
        gate.motion()
        gate.buttons()
        gate.wheels()
        gate.rejection_cleanup()
        gate.evidence["result"] = "PASS"
        print(f"PASS: independent settings acceptance. Evidence: {gate.run}", flush=True)
    except Exception as error:
        gate.evidence["result"] = "NOT ACCEPTED"
        gate.evidence["failure"] = repr(error)
        print(f"NOT ACCEPTED: {error!a}. Evidence: {gate.run}", flush=True)
        raise
    finally:
        gate.close()


if __name__ == "__main__":
    main()
