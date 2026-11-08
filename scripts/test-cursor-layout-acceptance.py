"""Independent finite cursor layout/idle gate; launch only after root READY.

Uses owned virtual targets and HWND-only pixel readback. No screen capture,
hardware input, OS input injection, Cargo, or producer implementation calls.
"""
import argparse
import copy
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
from pathlib import Path
import time
from PIL import Image

spec = importlib.util.spec_from_file_location("layout_wheel_base", Path(__file__).with_name("test-horizontal-wheel-acceptance.py"))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
eventually = base.eventually
user32 = base._base.user32
gdi = ctypes.WinDLL("gdi32", use_last_error=True)

class Rect(ctypes.Structure):
    _fields_ = [(k, wintypes.LONG) for k in ("left", "top", "right", "bottom")]

class BitmapHeader(ctypes.Structure):
    _fields_ = [("size", wintypes.DWORD), ("width", wintypes.LONG), ("height", wintypes.LONG),
                ("planes", wintypes.WORD), ("bits", wintypes.WORD), ("compression", wintypes.DWORD),
                ("image_size", wintypes.DWORD), ("xppm", wintypes.LONG), ("yppm", wintypes.LONG),
                ("used", wintypes.DWORD), ("important", wintypes.DWORD)]

user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(Rect)]
user32.GetWindowDC.argtypes = [wintypes.HWND]
user32.GetWindowDC.restype = wintypes.HDC
user32.ReleaseDC.argtypes = [wintypes.HWND, wintypes.HDC]
user32.PrintWindow.argtypes = [wintypes.HWND, wintypes.HDC, wintypes.UINT]
user32.IsWindowVisible.argtypes = [wintypes.HWND]
user32.GetWindowLongPtrW.argtypes = [wintypes.HWND, ctypes.c_int]
user32.GetWindowLongPtrW.restype = ctypes.c_ssize_t
user32.SetThreadDpiAwarenessContext.argtypes = [ctypes.c_void_p]
user32.SetThreadDpiAwarenessContext.restype = ctypes.c_void_p
gdi.CreateCompatibleDC.argtypes = [wintypes.HDC]
gdi.CreateCompatibleDC.restype = wintypes.HDC
gdi.CreateCompatibleBitmap.argtypes = [wintypes.HDC, ctypes.c_int, ctypes.c_int]
gdi.CreateCompatibleBitmap.restype = wintypes.HBITMAP
gdi.SelectObject.argtypes = [wintypes.HDC, wintypes.HGDIOBJ]
gdi.SelectObject.restype = wintypes.HGDIOBJ
gdi.BitBlt.argtypes = [wintypes.HDC, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int,
                       wintypes.HDC, ctypes.c_int, ctypes.c_int, wintypes.DWORD]
gdi.GetDIBits.argtypes = [wintypes.HDC, wintypes.HBITMAP, wintypes.UINT, wintypes.UINT,
                         ctypes.c_void_p, ctypes.POINTER(BitmapHeader), wintypes.UINT]
gdi.DeleteObject.argtypes = [wintypes.HGDIOBJ]
gdi.DeleteDC.argtypes = [wintypes.HDC]

def capture_owned(hwnd, path):
    """Ask only this owned HWND to paint; never use a desktop DC/ROI."""
    rect = Rect()
    assert user32.GetWindowRect(hwnd, ctypes.byref(rect))
    width, height = rect.right-rect.left, rect.bottom-rect.top
    assert 1 <= width <= 1024 and 1 <= height <= 1024, (width, height)
    source = user32.GetWindowDC(hwnd)
    assert source, "owned overlay window DC unavailable"
    dc = bitmap = old = None
    try:
        dc = gdi.CreateCompatibleDC(source)
        bitmap = gdi.CreateCompatibleBitmap(source, width, height)
        assert dc and bitmap
        old = gdi.SelectObject(dc, bitmap)
        assert user32.PrintWindow(hwnd, dc, 3), "owned overlay PrintWindow failed"
        gdi.SelectObject(dc, old)
        old = None
        header = BitmapHeader(ctypes.sizeof(BitmapHeader), width, -height, 1, 32, 0, 0, 0, 0, 0, 0)
        pixels = ctypes.create_string_buffer(width*height*4)
        assert gdi.GetDIBits(dc, bitmap, 0, height, pixels, ctypes.byref(header), 0) == height
        image = Image.frombytes("RGB", (width, height), pixels.raw, "raw", "BGRX")
        image.save(path)
        # Preserve even a failed readback, never pass from status hash alone.
        colors = len(image.getcolors(width*height) or [])
        assert colors >= 3, f"owned layered HWND readback blank/unsupported: {colors} colors; {path}"
        return {"path": str(path), "size": [width, height], "rgb_sha256": hashlib.sha256(image.tobytes()).hexdigest()}
    finally:
        if old: gdi.SelectObject(dc, old)
        if bitmap: gdi.DeleteObject(bitmap)
        if dc: gdi.DeleteDC(dc)
        user32.ReleaseDC(hwnd, source)

class LayoutGate(base.WheelGate):
    run_prefix = "acceptance-cursor-layout"

    def start(self):
        assert user32.SetThreadDpiAwarenessContext(ctypes.c_void_p(-4))
        self.user_names = ["Owned 東京🙂 label " + "界🙂"*70, "Owned x86 label 東京🙂"]
        super().start()
        wanted = {"label_position": "right", "label_font_size": 14.0,
                  "badge_shape": "rounded", "badge_position": "inline", "idle_hide_ms": None}
        assert all(self.defaults[k] == v for k, v in wanted.items()), self.defaults
        schema = next(t["inputSchema"] for t in self.evidence["tools"] if t["name"] == "routing_settings")
        def resolve(node):
            while "$ref" in node:
                value = schema
                for part in node["$ref"][2:].split("/"): value = value[part]
                node = value
            return node
        fields = resolve(schema["properties"]["settings"])["properties"]
        for field, values in (("label_position", {"left", "right", "above", "below"}),
                              ("badge_shape", {"rounded", "box", "circle"}),
                              ("badge_position", {"inline", "top_left", "top_right", "bottom_left", "bottom_right"})):
            assert set(resolve(fields[field])["enum"]) == values

    def overlay(self, user):
        o = self.state()["users"][user]["overlay"]
        rect = Rect()
        assert user32.GetWindowRect(o["hwnd"], ctypes.byref(rect))
        actual = [rect.left, rect.top, rect.right, rect.bottom]
        assert actual[:2] == o["render_origin"], (actual, o)
        if o["canvas_size"] == [0, 0]:
            # Hidden invalidated frames retain HWND allocation, but have no
            # rendered canvas. The visible-frame geometry oracle remains exact.
            assert not o["visible"] and o["painted_pixels"] == 0 and o["pixel_hash"] == 0
        else:
            assert [rect.right-rect.left, rect.bottom-rect.top] == o["canvas_size"]
            assert all(1 <= v <= 1024 for v in o["canvas_size"])
        assert [rect.left+o["hotspot"][0], rect.top+o["hotspot"][1]] == [o["x"], o["y"]]
        o["observed_native_visible"] = bool(user32.IsWindowVisible(o["hwnd"]))
        styles = user32.GetWindowLongPtrW(o["hwnd"], -20)
        assert styles & 0x80000 and styles & 0x20 and styles & 0x8000000
        assert o["no_activate"] and o["click_through"]
        return o

    def visible(self, user, visible):
        # Worker snapshot and HWND visibility are read separately during a tick.
        # Accept only when both observations reach the requested state.
        return eventually(lambda: self.overlay(user), lambda o:
            o["visible"] == visible and o["observed_native_visible"] == visible)

    def raw_output(self, key=None):
        buttons = [{"type": "legacy"} for _ in range(5)]
        buttons[3] = {"type": "disabled"} if key is None else {"type": "key", "key": key}
        self.client.tool("routing_output", {"operation": "configure", "user": 0,
            "settings": {"buttons": buttons, "keys": {}}})

    def rendered(self):
        records = []
        def sample(tag, **changes):
            options = dict(shape="ring", size=256, label=True, badge="AB東京🙂123")
            options.update(changes)
            self.apply(0, **options)
            o = self.visible(0, True)
            assert o["painted_pixels"] > 0 and o["pixel_hash"]
            assert len(o["rendered_label"]) <= 64
            assert o["label_truncated"] if options["label"] else o["rendered_label"] == ""
            assert "\ufffd" not in o["rendered_label"]
            assert min(o["canvas_size"]) >= 256
            capture = capture_owned(o["hwnd"], self.run/f"overlay-{tag}.png")
            records.append({"tag": tag, "overlay": o, "pixels": capture})
            return capture["rgb_sha256"]
        placements = [sample(position, label_position=position) for position in ("right", "left", "above", "below")]
        assert len(set(placements)) == 4, "label placements render identically"
        assert sample("font8", label_font_size=8) != sample("font48", label_font_size=48)
        shapes = [sample("badge-"+shape, badge_shape=shape) for shape in ("rounded", "box", "circle")]
        assert len(set(shapes)) == 3
        corners = [sample("position-"+p, badge_position=p) for p in ("inline", "top_left", "top_right", "bottom_left", "bottom_right")]
        assert len(set(corners)) == 5
        sample("badge-without-label", label=False)
        sample("maximum-wide-inline", badge="WWWWWWWW", badge_shape="circle", badge_position="inline",
               label_position="right", label_font_size=48)
        self.apply(1, shape="crosshair", size=256, label=False, badge="東京🙂", badge_shape="circle", badge_position="bottom_left")
        capture_owned(self.visible(1, True)["hwnd"], self.run/"overlay-x86-owner.png")
        before = self.overlay(0)
        self.input("motion", user=0, dx=3, dy=2)
        after = eventually(lambda: self.overlay(0), lambda o: [o["x"], o["y"]] == [before["x"]+3, before["y"]+2])
        assert after["paint_generation"] == before["paint_generation"], "position-only move repainted bitmap"
        assert after["pixel_hash"] == before["pixel_hash"]
        self.evidence["rendered_samples"] = records
        self.snapshot("owned-native-layout-max-size-font-badge-pixels-and-position-cache")

    def idle(self):
        for user in range(2): self.apply(user, idle_hide_ms=400, label=False)
        hidden = [self.visible(i, False) for i in range(2)]
        assert all(o["idle_hidden"] for o in hidden)
        saved = self.state()["user_settings"]
        cursor = self.report(0)["cursor"]
        self.tap(0, "f5")
        self.input("text", user=0, text="idle")
        time.sleep(.15)
        assert not self.overlay(0)["visible"] and self.overlay(0)["idle_hidden"]
        assert self.report(0)["cursor"] == cursor and self.state()["user_settings"] == saved
        for key in (None, 116):
            self.raw_output(key)
            self.visible(0, False)
            self.input("button", user=0, button="back", pressed=True)
            self.visible(0, True)
            other = self.overlay(1)
            assert not other["visible"] and not other["observed_native_visible"], "raw activity woke other seat"
            self.input("button", user=0, button="back", pressed=False)
            self.visible(0, False)
        self.raw_output(116)
        self.input("button", user=0, button="primary", pressed=True)
        self.visible(0, True)
        eventually(lambda: self.report(0), lambda r: r["buttons"] & 1)
        time.sleep(.65)
        assert self.overlay(0)["visible"], "actual held drag idled out"
        self.input("button", user=0, button="primary", pressed=False)
        self.visible(0, False)
        self.input("move_to", user=0, x=self.points[0][0], y=self.points[0][1])
        self.visible(0, True)
        self.input("pause")
        self.visible(0, False)
        assert self.state()["paused"]
        self.input("resume")
        assert not self.overlay(0)["visible"]
        self.apply(0, idle_hide_ms=400, visible=False)
        self.input("motion", user=0, dx=1, dy=0)
        assert not self.overlay(0)["visible"]
        self.snapshot("raw-mouse-idle-isolation-keyboard-not-wake-native-held-drag-and-pause")

    def pipeline(self):
        for user in range(2): self.apply(user, idle_hide_ms=400)
        for user in range(2): self.visible(user, False)
        self.input("move_to", user=0, x=self.points[0][0], y=self.points[0][1])
        self.visible(0, True)
        assert not self.overlay(1)["visible"]
        self.visible(0, False)
        clip = {"version": 1, "events": [{"at_micros": 0, "action": {"type": "move_to", "user": 0,
                    "x": self.points[0][0]+1, "y": self.points[0][1]}}]}
        self.client.tool("automation_clip", {"action": "load", "clip": clip})
        self.client.tool("automation_play", {"loops": 1, "speed": 1.0})
        self.visible(0, True)
        self.client.tool("routing_replication", {"edges": [{"source": 0, "target": 1, "mouse": True, "keyboard": False, "text": False}]})
        for user in range(2): self.visible(user, False)
        self.input("motion", user=0, dx=1, dy=0)
        for user in range(2): self.visible(user, True)
        for user in range(2): self.visible(user, False)
        self.apply(0, idle_hide_ms=400, badge="Reset")
        self.visible(0, True)
        assert not self.overlay(1)["visible"]
        self.client.tool("routing_replication", {"edges": []})
        self.snapshot("move-to-replay-replica-and-selected-settings-idle-reset")

    def boundaries(self):
        self.apply(0, idle_hide_ms=400)
        self.key(1, "f6", True)
        before = self.state()["user_settings"]
        for field, bad in (("size", 257), ("size", 11), ("label_font_size", 7), ("label_font_size", 49),
                           ("idle_hide_ms", 249), ("idle_hide_ms", 600001), ("label_position", "diagonal"),
                           ("badge_shape", "triangle"), ("badge_position", "center"), ("badge", "123456789")):
            candidate = copy.deepcopy(before[0]["settings"])
            candidate[field] = bad
            self.client.tool("routing_settings", {"user": 0, "settings": candidate}, error=True)
            assert self.state()["user_settings"] == before
            assert 117 in self.report(1)["held_keys"]
        self.key(1, "f6", False)
        self.snapshot("invalid-appearance-boundaries-preserve-settings-and-unrelated-native-hold")

def main():
    parser = argparse.ArgumentParser()
    for name in ("executable", "x86-executable", "runtime"): parser.add_argument("--"+name, type=Path, required=True)
    parser.add_argument("--ready", action="store_true")
    parser.add_argument("--groups", default="rendered,idle,pipeline,boundaries")
    args = parser.parse_args()
    assert args.ready, "Wait for root READY and exclusive native slot"
    gate = LayoutGate(args)
    try:
        gate.start()
        for group in args.groups.split(","):
            assert group in ("rendered", "idle", "pipeline", "boundaries")
            getattr(gate, group)()
            print(f"Native accepted {group}", flush=True)
        overlays = [u["overlay"]["hwnd"] for u in gate.state()["users"]]
        gate.client.tool("routing_stop")
        gate.released()
        eventually(lambda: [user32.IsWindow(h) for h in overlays], lambda live: not any(live))
        gate.snapshot("stop-native-release-and-overlay-destruction")
        gate.evidence["result"] = "PASS"
    except Exception as error:
        gate.evidence.update(result="NOT ACCEPTED", failure=repr(error))
        raise
    finally:
        gate.close()
        print(f"{gate.evidence.get('result')}: {gate.run}", flush=True)

if __name__ == "__main__": main()
