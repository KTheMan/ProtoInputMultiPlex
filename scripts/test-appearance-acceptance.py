"""Finite virtual-only appearance/bounds gate against owned x64/x86 targets."""
import argparse
import copy
import ctypes
from ctypes import wintypes
import importlib.util
from pathlib import Path
import time

_spec = importlib.util.spec_from_file_location("settings_gate", Path(__file__).with_name("test-settings-acceptance.py"))
_module = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)
eventually = _module.eventually
user32 = _module._module.user32


class Rect(ctypes.Structure):
    _fields_ = [(field, wintypes.LONG) for field in ("left", "top", "right", "bottom")]


user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(Rect)]
user32.GetWindowLongPtrW.argtypes = [wintypes.HWND, ctypes.c_int]
user32.GetWindowLongPtrW.restype = ctypes.c_ssize_t
user32.IsWindowVisible.argtypes = [wintypes.HWND]
user32.SendMessageTimeoutW.argtypes = [wintypes.HWND, wintypes.UINT, ctypes.c_size_t,
                                     ctypes.c_ssize_t, wintypes.UINT, wintypes.UINT,
                                     ctypes.POINTER(ctypes.c_size_t)]
user32.SendMessageTimeoutW.restype = ctypes.c_ssize_t
user32.SetThreadDpiAwarenessContext.argtypes = [ctypes.c_void_p]
user32.SetThreadDpiAwarenessContext.restype = ctypes.c_void_p
assert user32.SetThreadDpiAwarenessContext(ctypes.c_void_p(-4)), "runner physical-pixel DPI context"


class AppearanceGate(_module.SettingsGate):
    tool_names = _module.SettingsGate.tool_names | {"routing_displays"}
    run_prefix = "acceptance-appearance"

    def inventory_without_backend(self):
        runtime = self.run / "empty-runtime"
        runtime.mkdir()
        client = _module._module.Client(self.args.executable.resolve(), runtime, self.run / "inventory.stderr.log")
        try:
            assert not list(runtime.iterdir())
            inventory = client.tool("routing_displays")
            assert inventory == client.tool("routing_displays"), "display identities changed between adjacent read-only calls"
            assert not client.tool("routing_state")["running"]
            assert not list(runtime.iterdir()), "read-only inventory mutated empty runtime"
            assert inventory["monitors"]
            self.evidence["discovery_without_backend"] = inventory
        finally:
            client.close()

    def discover(self):
        super().discover()
        self.displays = self.client.tool("routing_displays")
        stable = [m for m in self.displays["monitors"] if m["stable_id"]]
        assert stable, "no connected stable monitor identity available for this gate"
        self.monitor = next((m for m in stable if m["primary"]), stable[0])
        assert not self.monitor["id"].startswith("unverified-adapter:")
        schema = next(t for t in self.evidence["tools"] if t["name"] == "routing_settings")["inputSchema"]
        def resolve(node):
            while "$ref" in node:
                ref = node["$ref"]
                node = schema
                for part in ref[2:].split("/"):
                    node = node[part]
            return node
        def choices(node):
            node = resolve(node)
            values = set(node.get("enum", []))
            if "const" in node:
                values.add(node["const"])
            for child in node.get("oneOf", []):
                values.update(choices(child))
            return values
        settings = resolve(schema["properties"]["settings"])["properties"]
        assert set(resolve(settings["shape"])["enum"]) == {"arrow", "crosshair", "i_beam", "ring"}
        assert choices(settings["bounds_mode"]) == {"legacy", "desktop", "window", "monitor", "region"}
        assert {"badge", "monitor_id", "monitor_work_area", "region"} <= set(settings)
        assert self.defaults["shape"] == "arrow" and self.defaults["badge"] is None
        assert self.defaults["bounds_mode"] == "legacy" and self.defaults["monitor_id"] is None and self.defaults["region"] is None
        self.snapshot("current-tool-schemas-full-appearance-bounds-defaults")

    def overlay(self, user, native=True):
        def aligned():
            state = self.state()["users"][user]
            overlay = state["overlay"]
            if native and [overlay["x"], overlay["y"]] != self.report(user)["cursor"]:
                return None
            return state
        state = eventually(aligned, lambda v: v is not None)
        o = state["overlay"]
        rect = Rect()
        assert user32.GetWindowRect(o["hwnd"], ctypes.byref(rect))
        self.evidence["last_overlay_geometry"] = {"state": state, "native_rect": [rect.left, rect.top, rect.right, rect.bottom]}
        assert [rect.left, rect.top] == o["render_origin"], (o["shape"], [rect.left, rect.top], o["render_origin"], o["hotspot"], [o["x"], o["y"]])
        assert [rect.left + o["hotspot"][0], rect.top + o["hotspot"][1]] == [o["x"], o["y"]]
        assert bool(user32.IsWindowVisible(o["hwnd"])) == o["visible"]
        styles = user32.GetWindowLongPtrW(o["hwnd"], -20)
        assert styles & 0x80000 and styles & 0x20 and styles & 0x8000000
        assert o["no_activate"] and o["click_through"]
        for message, expected in ((0x84, -1), (0x21, 3)):
            result = ctypes.c_size_t()
            position = ((o["y"] & 0xFFFF) << 16) | (o["x"] & 0xFFFF)
            assert user32.SendMessageTimeoutW(o["hwnd"], message, 0, position, 2, 1000, ctypes.byref(result))
            assert ctypes.c_ssize_t(result.value).value == expected
        return state

    def appearance(self):
        for user in range(2):
            hashes = {}
            for shape in ("arrow", "crosshair", "i_beam", "ring"):
                self.apply(user, shape=shape, label=False, badge=None)
                state = self.overlay(user)
                o = state["overlay"]
                assert o["shape"] == shape and o["painted_pixels"] > 0 and o["pixel_hash"] != 0
                hashes[shape] = o["pixel_hash"]
            assert len(set(hashes.values())) == 4, hashes
            self.evidence["checks"][f"distinct-rendered-shapes-{user}"] = hashes
            for size in (12, 64):
                self.apply(user, shape="ring", size=size, label=False)
                o = self.overlay(user)["overlay"]
                assert o["hotspot"] == [size//2, size//2] and o["painted_pixels"] > 0
            self.apply(user, shape="ring", label=False)
            plain_hash = self.overlay(user)["overlay"]["pixel_hash"]
            self.apply(user, shape="ring", label=True, badge="AB東京🙂123")
            badged = self.overlay(user)["overlay"]
            assert badged["badge"] == "AB東京🙂123" and badged["pixel_hash"] != plain_hash
            self.apply(user, visible=False, shape="crosshair")
            hidden = self.overlay(user)["overlay"]
            assert not hidden["visible"] and hidden["painted_pixels"] == 0 and hidden["pixel_hash"] == 0
            self.apply(user, visible=True, shape="crosshair")
            assert self.overlay(user)["overlay"]["painted_pixels"] > 0
        self.snapshot("native-shapes-hotspots-sizes-badge-label-hide-show")

    def clamp(self, user, bounds, native=False):
        assert self.state()["users"][user]["movement_bounds"] == bounds
        self.input("move_to", user=user, x=bounds["right"]+200, y=bounds["bottom"]+200)
        eventually(self.state, lambda s: [s["users"][user]["overlay"]["x"], s["users"][user]["overlay"]["y"]] == [bounds["right"]-1, bounds["bottom"]-1])
        self.overlay(user, native)
        self.input("motion", user=user, dx=-30000, dy=-30000)
        eventually(self.state, lambda s: [s["users"][user]["overlay"]["x"], s["users"][user]["overlay"]["y"]] == [bounds["left"], bounds["top"]])
        self.overlay(user, native)

    def bounds(self):
        for mode, extras, expected in (
            ("desktop", {}, self.displays["desktop"]),
            ("legacy", {"lock_window": False}, self.displays["desktop"]),
            ("monitor", {"monitor_id": self.monitor["id"]}, self.monitor["bounds"]),
            ("monitor", {"monitor_id": self.monitor["id"], "monitor_work_area": True}, self.monitor["work_area"]),
        ):
            self.apply(0, bounds_mode=mode, **extras)
            self.clamp(0, expected)
        for user in range(2):
            self.apply(user, bounds_mode="desktop")
            self.anchored(user)
            for mode, extra in (("window", {}), ("legacy", {"lock_window": True})):
                self.apply(user, bounds_mode=mode, **extra)
                state = self.state()["users"][user]
                assert state["movement_bounds"] == state["bounds"]
                self.clamp(user, state["movement_bounds"], native=False)
                self.apply(user, bounds_mode="desktop")
                self.anchored(user)
        self.snapshot("desktop-window-monitor-workarea-legacy-absolute-relative-clamp")

    def region_lifecycle(self):
        x, y = self.points[0]
        region = {"left": x+4, "top": y+4, "right": x+24, "bottom": y+24}
        for user in range(2):
            self.key(user, "f5", True)
            self.input("button", user=user, button="primary", pressed=True)
        eventually(self.reports, lambda rs: all(116 in r["held_keys"] and r["buttons"] == 1 for r in rs))
        other = self.report(1)
        self.apply(0, shape="ring", bounds_mode="region", region=region)
        eventually(lambda: self.report(0), lambda r: r["cursor"] == [x+4, y+4] and not r["held_keys"] and r["buttons"] == 0)
        assert self.report(1)["buttons"] == other["buttons"] and self.report(1)["held_keys"] == other["held_keys"]
        self.clamp(0, region, native=True)
        self.snapshot("region-immediate-native-clamp-selected-release-other-retention")
        current_settings = self.state()["user_settings"]
        self.key(0, "f5", True)
        self.input("button", user=0, button="primary", pressed=True)
        eventually(lambda: self.report(0), lambda r: 116 in r["held_keys"] and r["buttons"] == 1)
        before_state, before_probes = self.state(), self.reports()
        invalid_changes = [{"badge": "123456789"}, {"badge": "bad\n"},
            {"bounds_mode": "monitor", "monitor_id": None},
            {"bounds_mode": "monitor", "monitor_id": "missing-monitor://acceptance-only"},
            {"bounds_mode": "region", "region": {"left": x, "right": x, "top": y, "bottom": y+1}},
            {"bounds_mode": "region", "region": {"left": -999999, "top": -999999, "right": -999998, "bottom": -999998}},
            {"bounds_mode": "region", "monitor_id": "missing-monitor://acceptance-only", "region": region}]
        for changes in invalid_changes:
            settings = copy.deepcopy(current_settings[0]["settings"])
            settings.update(changes)
            self.client.tool("routing_settings", {"user": 0, "settings": settings}, error=True)
            after = self.state()
            assert after["user_settings"] == before_state["user_settings"]
            assert after["users"][0]["movement_bounds"] == region
            for index, r in enumerate(self.reports()):
                assert r["cursor"] == before_probes[index]["cursor"] and r["held_keys"] == before_probes[index]["held_keys"] and r["buttons"] == before_probes[index]["buttons"]
            assert after["users"][0]["overlay"]["pixel_hash"] == before_state["users"][0]["overlay"]["pixel_hash"]
        self.snapshot("runtime-rejects-invalid-badge-region-disconnected-monitor-before-mutation")
        self.input("release_all")
        self.apply(0, bounds_mode="desktop")
        self.anchored(0)
        self.input("pause")
        paused_cursor = self.report(0)["cursor"]
        self.apply(0, shape="crosshair", bounds_mode="region", region=region)
        assert [self.state()["users"][0]["overlay"][k] for k in ("x", "y")] == [x+4, y+4]
        time.sleep(.2)
        assert self.report(0)["cursor"] == paused_cursor, "paused bounds apply sent native motion"
        self.overlay(0, native=False)
        self.input("resume")
        eventually(lambda: self.report(0), lambda r: r["cursor"] == [x+4, y+4])
        self.overlay(0)
        self.snapshot("paused-bounds-apply-defers-native-until-resume-aligned")
        overlays = [u["overlay"]["hwnd"] for u in self.state()["users"]]
        self.client.tool("routing_stop")
        self.released()
        assert not any(user32.IsWindow(h) for h in overlays)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    gate = AppearanceGate(parser.parse_args())
    try:
        gate.inventory_without_backend()
        gate.start()
        gate.discover()
        gate.appearance()
        gate.bounds()
        gate.region_lifecycle()
        gate.evidence["result"] = "PASS"
        print(f"PASS: native appearance and bounds. Evidence: {gate.run}", flush=True)
    except Exception as error:
        gate.evidence["result"] = "NOT ACCEPTED"
        gate.evidence["failure"] = repr(error)
        print(f"NOT ACCEPTED: {error!a}. Evidence: {gate.run}", flush=True)
        raise
    finally:
        gate.close()


if __name__ == "__main__":
    main()
