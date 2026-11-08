"""Independent finite annotation gate: virtual seats and owned x64/x86 applications.

PNG pixels are decoded independently with Pillow. No hardware/global input APIs.
Run only after the parent grants the exclusive live slot and freezes both builds.
"""
import argparse
import copy
import ctypes
from ctypes import wintypes
import importlib.util
import json
from pathlib import Path
import time
from PIL import Image

_spec = importlib.util.spec_from_file_location("appearance_gate", Path(__file__).with_name("test-appearance-acceptance.py"))
_module = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)
eventually, user32, Rect = _module.eventually, _module.user32, _module.Rect
user32.GetClientRect.argtypes = [wintypes.HWND, ctypes.POINTER(Rect)]
user32.ShowWindow.argtypes = [wintypes.HWND, ctypes.c_int]
user32.ClientToScreen.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.POINT)]
user32.GetForegroundWindow.restype = wintypes.HWND


class AnnotationGate(_module.AppearanceGate):
    run_prefix = "acceptance-annotation"
    colors = [(24, 96, 160), (192, 32, 80)]

    def fixture_arguments(self, index):
        r, g, b = self.colors[index]
        return ["--capture-pattern", str(r << 16 | g << 8 | b)]

    def before_selection(self):
        before = self.annotation()
        self.client.tool("annotation", {"operation":"export", "user":0, "kind":"selected_target", "path":str(self.run/"unfocused.png")}, error=True)
        after = self.annotation()
        assert not (self.run/"unfocused.png").exists()
        assert after["users"] == before["users"] and after["revision"] == before["revision"]
        assert self.state()["running"] and all(not u["pid"] for u in self.state()["users"])
        self.evidence["checks"]["no-selected-window-capture-rejected-without-mutation"]={"before":before,"after":after}

    def annotation(self, operation="inspect", **fields):
        return self.client.tool("annotation", {"operation": operation, **fields})["annotation"]

    def configure(self, user, **changes):
        settings = copy.deepcopy(self.annotation_defaults)
        settings.update(changes)
        result = self.annotation("configure", user=user, settings=settings)
        assert result["error"] is None, result
        assert result["users"][user]["settings"] == settings
        return settings

    def exported(self, name, kind="ink_only", user=0):
        path = self.run / (name + ".png")
        result = self.annotation("export", user=user, path=str(path), kind=kind)
        assert result["error"] is None, result
        with Image.open(path) as source:
            assert source.mode == "RGBA", source.mode
            image = source.copy()
        assert list(image.size) == [result["export"]["width"], result["export"]["height"]]
        self.evidence.setdefault("exports", {})[name] = result
        return image

    def pixel(self, image, point):
        return image.getpixel((point[0] - self.desktop["left"], point[1] - self.desktop["top"]))

    def go(self, user, point):
        self.input("move_to", user=user, x=point[0], y=point[1])
        eventually(lambda: self.report(user), lambda r: r["cursor"] == list(point))

    def stroke(self, user, start, end, button="primary"):
        self.go(user, start)
        self.input("button", user=user, button=button, pressed=True)
        assert self.annotation()["users"][user]["open"]
        self.go(user, end)
        self.input("button", user=user, button=button, pressed=False)
        assert not self.annotation()["users"][user]["open"]

    def discover_annotation(self):
        self.defaults = self.state()["user_settings"][0]["settings"]
        state = self.annotation()
        self.annotation_defaults = state["users"][0]["settings"]
        assert self.annotation_defaults == {"tool": "off", "activator": "primary", "color": [96, 165, 250], "alpha": 255,
                                            "width": 6, "clear_key": None, "clear_all": False}
        assert all({k: v for k, v in u["settings"].items() if k != "color"} == {k: v for k, v in self.annotation_defaults.items() if k != "color"}
                   and not u["segments"] and not u["open"] for u in state["users"])
        schema = next(t["inputSchema"] for t in self.evidence["tools"] if t["name"] == "annotation")
        serialized = json.dumps(schema)
        for term in ("configure", "clear_user", "clear_all", "export", "ink_only", "selected_target", "eraser", "x2", "clear_key"):
            assert '"' + term + '"' in serialized, term
        self.desktop = self.client.tool("routing_displays")["desktop"]
        self.windows = [next(w for w in self.client.tool("routing_windows")["windows"] if w["pid"] == p.pid and w["title"].startswith("ProtoInputMultiPlex Automation Test")) for p in self.probes]
        self.snapshot("fourteen-tools-typed-annotation-defaults")

    def activators(self):
        names = ["primary", "secondary", "middle", "back", "forward"]
        activators = ["primary", "secondary", "middle", "x1", "x2"]
        for user in (0, 1):
            self.apply(user, button_mapping=[None] * 5)
            for index, (button, activator) in enumerate(zip(names, activators)):
                x, y = self.points[user]
                self.configure(user, tool="brush", activator=activator, color=[0, 0, 0], width=8)
                baseline = self.report(user)["mouse_event_count"]
                self.stroke(user, (x-20, y-40+index*20), (x+40, y-40+index*20), button)
                assert self.report(user)["mouse_event_count"] == baseline, "source activator leaked through disabled map"
                assert not self.report(user)["buttons"]
            self.apply(user)
            self.configure(user, tool="brush", color=[0, 0, 0])
            self.edge(user, 1, 1)
            baseline = self.report(user)["key_event_count"]
            self.tap(user, "f5")
            eventually(lambda: self.report(user), lambda r: r["key_event_count"] == baseline+2)
        image = self.exported("five-activators-black")
        for user in (0, 1):
            x, y = self.points[user]
            for index in range(5):
                assert self.pixel(image, (x+10, y-40+index*20)) == (0, 0, 0, 255)
        overlay = eventually(lambda: self.annotation()["overlay"], lambda o: o and o["visible"] and o["painted_pixels"] > 0)
        assert user32.IsWindow(overlay["hwnd"])
        styles = user32.GetWindowLongPtrW(overlay["hwnd"], -20)
        assert styles & 0x80000 and styles & 0x20 and styles & 0x8000000
        assert overlay["click_through"] and overlay["no_activate"]
        self.snapshot("five-source-activators-native-consumed-black-pixels-normal-keys-buttons")

    def raster_ownership(self):
        self.annotation("clear_all")
        x, y = self.points[0]
        self.configure(0, tool="brush", color=[30, 90, 150], alpha=128, width=8)
        self.stroke(0, (x-30, y-20), (x+50, y-20))
        image = self.exported("straight-alpha-128")
        assert self.pixel(image, (x+10, y-20)) == (30, 90, 150, 128)
        assert self.pixel(image, (x+10, y-40))[3] == 0
        self.annotation("clear_all")
        self.configure(0, tool="brush", alpha=0)
        self.stroke(0, (x-30, y), (x+50, y))
        assert self.exported("alpha-zero").getchannel("A").getbbox() is None
        self.annotation("clear_all")
        self.configure(0, tool="brush", color=[220, 20, 20], width=8)
        self.configure(1, tool="brush", color=[20, 220, 20], width=8)
        self.stroke(0, (x-40, y), (x+60, y))
        bx, by = self.points[1]
        self.stroke(1, (bx-40, by), (bx+60, by))
        self.configure(0, tool="eraser", width=20)
        self.stroke(0, (x+10, y-15), (x+10, y+15))
        image = self.exported("partial-owner-erase")
        assert self.pixel(image, (x+10, y))[3] == 0
        assert self.pixel(image, (x-25, y)) == (220, 20, 20, 255)
        assert self.pixel(image, (x+45, y)) == (220, 20, 20, 255)
        assert self.pixel(image, (bx+10, by)) == (20, 220, 20, 255)
        self.annotation("clear_user", user=0)
        image = self.exported("clear-mine")
        assert self.pixel(image, (x-25, y))[3] == 0
        assert self.pixel(image, (bx+10, by)) == (20, 220, 20, 255)
        self.configure(1, tool="brush", clear_key=119)
        baseline = self.report(1)["key_event_count"]
        self.tap(1, "f8")
        assert self.annotation()["users"][1]["segments"] == 0
        assert self.report(1)["key_event_count"] == baseline
        self.configure(0, tool="brush")
        self.stroke(0, (x-30, y), (x+50, y))
        self.stroke(1, (bx-30, by), (bx+50, by))
        self.configure(1, tool="brush", clear_key=119, clear_all=True)
        self.tap(1, "f8")
        assert all(not u["segments"] for u in self.annotation()["users"])
        assert self.exported("clear-key-all").getchannel("A").getbbox() is None
        self.snapshot("straight-rgba-alpha-partial-erase-owner-clear-and-clear-key")

    def replication_policy(self):
        x, y = self.points[0]
        bx, by = self.points[1]
        self.annotation("clear_all")
        self.configure(0, tool="brush", color=[255, 0, 0])
        self.configure(1)
        self.go(1, (bx, by))
        self.client.tool("routing_replication", {"edges": [{"source": 0, "target": 1}]})
        # Absolute moves also fan out, so anchor before enabling the graph and
        # use relative movement to preserve each target's independent location.
        self.client.tool("routing_replication", {"edges": []})
        self.go(0, (x, y))
        self.go(1, (bx, by))
        self.client.tool("routing_replication", {"edges": [{"source": 0, "target": 1}]})
        baseline = self.reports()
        self.input("button", user=0, button="primary", pressed=True)
        self.input("motion", user=0, dx=40, dy=0)
        self.input("button", user=0, button="primary", pressed=False)
        eventually(lambda: self.report(1), lambda r: r["mouse_event_count"] == baseline[1]["mouse_event_count"]+2)
        assert self.report(0)["mouse_event_count"] == baseline[0]["mouse_event_count"]
        assert self.annotation()["users"][0]["segments"] > 0 and self.annotation()["users"][1]["segments"] == 0
        self.configure(1, tool="brush", color=[0, 0, 255])
        baseline = self.reports()
        self.input("button", user=0, button="primary", pressed=True)
        self.input("motion", user=0, dx=40, dy=0)
        self.input("button", user=0, button="primary", pressed=False)
        assert all(self.report(i)["mouse_event_count"] == baseline[i]["mouse_event_count"] for i in (0, 1))
        image = self.exported("replication-destination-policies")
        assert self.pixel(image, (x+60, y)) == (255, 0, 0, 255)
        assert self.pixel(image, (bx+60, by)) == (0, 0, 255, 255)
        self.client.tool("routing_replication", {"edges": []})
        self.snapshot("replication-destination-off-delivers-brush-consumes-independently")

    def lifecycle(self):
        self.annotation("clear_all")
        self.configure(0, tool="brush")
        self.anchored(0)
        baseline = self.report(0)["mouse_event_count"]
        self.input("button", user=0, button="primary", pressed=True)
        self.configure(0)
        self.input("button", user=0, button="primary", pressed=False)
        assert self.report(0)["mouse_event_count"] == baseline
        assert not self.annotation()["users"][0]["open"]
        for command in ("pause", "release_all", "settings"):
            self.configure(0, tool="brush")
            self.anchored(0)
            self.input("button", user=0, button="primary", pressed=True)
            if command == "pause":
                self.input("pause")
            elif command == "release_all":
                self.input("release_all")
            else:
                self.apply(0)
            before = self.annotation()["users"][0]
            assert not before["open"], command
            if command == "pause":
                self.input("resume")
            self.input("motion", user=0, dx=10, dy=0)
            assert self.annotation()["users"][0]["segments"] == before["segments"], command
            self.input("button", user=0, button="primary", pressed=False)
        self.snapshot("paired-up-after-off-pause-release-settings-end-strokes")

    def capture(self):
        self.annotation("clear_all")
        for i in (0, 1):
            self.configure(i)
            self.anchored(i)
        report = self.report(0)
        assert report["capture_color"] == list(self.colors[0])
        hwnd = report["capture_hwnd"]
        rect = Rect()
        assert user32.GetClientRect(hwnd, ctypes.byref(rect))
        image = self.exported("selected-child-pattern", "selected_target")
        info = self.evidence["exports"]["selected-child-pattern"]["export"]
        assert info["hwnd"] == hwnd and info["pid"] == self.probes[0].pid
        assert image.size == (rect.right, rect.bottom)
        assert image.getpixel((8, 8)) == (255, 255, 255, 255)
        assert image.getpixel((40, 40)) == (*self.colors[0], 255)
        other = self.windows[1]
        first = self.windows[0]
        assert user32.SetWindowPos(other["hwnd"], wintypes.HWND(-1), 30, 40, 420, 360, 0x10)
        overlap = self.exported("selected-child-under-unrelated-overlap", "selected_target")
        assert overlap.tobytes() == image.tobytes(), "unrelated overlapping window contaminated selected HWND capture"
        assert user32.SetWindowPos(other["hwnd"], wintypes.HWND(-1), 490, 40, 420, 360, 0x10)
        self.configure(0, tool="brush", color=[255, 0, 0], alpha=128, width=8)
        x, y = self.points[0]
        self.stroke(0, (x-20, y), (x+40, y))
        composition = self.exported("selected-child-composition", "selected_target")
        # Derive the exact local position from client-to-screen, independently.
        point = wintypes.POINT(0, 0)
        assert user32.ClientToScreen(hwnd, ctypes.byref(point))
        expected = tuple((s*128+b*127+127)//255 for s, b in zip((255, 0, 0), self.colors[0]))+(255,)
        assert composition.getpixel((x+10-point.x, y-point.y)) == expected
        self.key(1, "f5", True)
        eventually(lambda: self.report(1), lambda r: 116 in r["held_keys"])
        before = self.annotation()
        for fields in ({"operation": "configure", "user": 0, "settings": {**self.annotation_defaults, "width": 0}},
                       {"operation": "export", "user": 99, "kind": "selected_target", "path": str(self.run/"unknown.png")},
                       {"operation": "export", "user": 0, "kind": "ink_only", "path": str(self.run/"missing"/"bad.png")}):
            self.client.tool("annotation", fields, error=True)
            current = self.annotation()
            assert current["users"] == before["users"] and current["revision"] == before["revision"]
            assert 116 in self.report(1)["held_keys"] and self.state()["running"]
        user32.ShowWindow(first["hwnd"], 7)
        try:
            self.client.tool("annotation", {"operation": "export", "user": 0, "kind": "selected_target", "path": str(self.run/"minimized.png")}, error=True)
            assert not (self.run/"minimized.png").exists()
            assert self.annotation()["users"] == before["users"] and 116 in self.report(1)["held_keys"]
        finally:
            user32.ShowWindow(first["hwnd"], 4)
            assert not user32.IsIconic(first["hwnd"]), "owned minimized fixture failed to restore"
            assert user32.SetWindowPos(first["hwnd"], wintypes.HWND(-1), 30, 40, 420, 360, 0x10)
        self.key(1, "f5", False)
        self.snapshot("exact-child-printwindow-rgba-composition-unrelated-overlap-and-nonfatal-errors")

    def stop(self):
        self.configure(0)
        self.input("move_to",user=0,x=self.points[0][0],y=self.points[0][1])
        self.input("button",user=0,button="primary",pressed=True)
        self.input("button",user=0,button="primary",pressed=False)
        self.configure(0, tool="brush")
        self.anchored(0)
        self.input("button", user=0, button="primary", pressed=True)
        self.key(1, "f5", True)
        eventually(lambda: self.report(1), lambda r: 116 in r["held_keys"])
        state = self.state()
        handles = [u["overlay"]["hwnd"] for u in state["users"]]
        handles.append(self.annotation()["overlay"]["hwnd"])
        self.client.tool("routing_stop")
        self.released()
        eventually(lambda: [bool(user32.IsWindow(h)) for h in handles], lambda live: not any(live))
        self.snapshot("stop-releases-normal-holds-destroys-cursor-and-annotation-windows")

    def supplement(self):
        # A focused tail completes lifecycle criteria without repeating the
        # already recorded five-activator/RGBA/capture matrix.
        first = self.windows[0]
        user32.ShowWindow(first["hwnd"], 7)
        try:
            self.client.tool("annotation", {"operation":"export", "user":0, "kind":"selected_target", "path":str(self.run/"minimized.png")}, error=True)
        finally:
            user32.ShowWindow(first["hwnd"], 4)
            assert not user32.IsIconic(first["hwnd"])
            assert user32.SetWindowPos(first["hwnd"], wintypes.HWND(-1), 30,40,420,360,0x10)
        self.input("move_to",user=0,x=self.points[0][0],y=self.points[0][1])
        self.input("button",user=0,button="primary",pressed=True)
        self.input("button",user=0,button="primary",pressed=False)
        self.configure(0, tool="brush")
        self.anchored(0)
        child_bounds = self.state()["users"][0]["bounds"]
        self.input("button", user=0, button="primary", pressed=True)
        root_point = (first["client"]["left"]+390, first["client"]["top"]+290)
        self.input("move_to", user=0, x=root_point[0], y=root_point[1])
        self.input("button", user=0, button="secondary", pressed=True)
        self.input("button", user=0, button="secondary", pressed=False)
        changed = eventually(self.state, lambda s: s["users"][0]["bounds"] != child_bounds and not s["annotation"]["users"][0]["open"])
        self.exported("changed-selected-root", "selected_target")
        assert self.evidence["exports"]["changed-selected-root"]["export"]["hwnd"] == first["hwnd"]
        before = changed["annotation"]["users"][0]["segments"]
        self.input("motion", user=0, dx=-5, dy=0)
        assert self.annotation()["users"][0]["segments"] == before
        self.input("button", user=0, button="primary", pressed=False)
        self.configure(0)
        self.input("move_to", user=0, x=self.points[0][0], y=self.points[0][1])
        self.input("button", user=0, button="primary", pressed=True)
        self.input("button", user=0, button="primary", pressed=False)
        self.annotation("clear_all")
        x,y = self.points[0]
        bx,by = self.points[1]
        self.configure(1, tool="brush", color=[0,220,0], width=8)
        self.stroke(1,(bx-40,by),(bx+60,by))
        # Cross the other owner's stroke with user0's eraser: it must survive.
        self.configure(0, tool="eraser", width=20)
        self.input("move_to", user=0,x=bx+10,y=by-15)
        self.input("button",user=0,button="primary",pressed=True)
        self.input("move_to",user=0,x=bx+10,y=by+15)
        self.input("button",user=0,button="primary",pressed=False)
        assert self.pixel(self.exported("eraser-cross-other-owner"),(bx+10,by)) == (0,220,0,255)
        self.configure(0, tool="brush", color=[220,0,0], width=8)
        self.configure(1, tool="eraser", width=20)
        self.input("move_to",user=0,x=x,y=y-15)
        self.go(1,(bx+10,by-15))
        self.client.tool("routing_replication", {"edges":[{"source":0,"target":1}]})
        self.input("button",user=0,button="primary",pressed=True)
        self.input("motion",user=0,dx=0,dy=30)
        self.input("button",user=0,button="primary",pressed=False)
        image = self.exported("replication-destination-eraser")
        assert self.pixel(image,(bx+10,by))[3] == 0
        assert self.pixel(image,(bx-25,by)) == (0,220,0,255)
        assert self.pixel(image,(x,y)) == (220,0,0,255)
        self.client.tool("routing_replication", {"edges":[]})
        self.snapshot("restored-fixture-target-change-ends-stroke-owner-eraser-and-fanout-local-eraser")
        self.stop()

    def eof(self):
        self.configure(0,tool="brush")
        self.anchored(0)
        self.input("button",user=0,button="primary",pressed=True)
        self.input("motion",user=0,dx=20,dy=0)
        self.key(1,"f5",True)
        eventually(lambda:self.report(1),lambda r:116 in r["held_keys"])
        state=self.state()
        handles=[u["overlay"]["hwnd"] for u in state["users"]]+[self.annotation()["overlay"]["hwnd"]]
        self.client.close()
        self.client=None
        self.released()
        eventually(lambda:[bool(user32.IsWindow(h)) for h in handles],lambda live:not any(live))
        self.evidence["checks"]["stdio-eof-releases-holds-destroys-open-stroke-canvas"]={"before":state,"handles":handles,"after_probes":self.reports()}

    def overlay_boundary(self):
        foreground = user32.GetForegroundWindow()
        self.configure(0,tool="brush",color=[0,0,0],width=8)
        x,y=self.points[0]
        self.stroke(0,(x,y),(x+30,y))
        overlay=eventually(lambda:self.annotation()["overlay"],lambda o:o and o["painted_pixels"]>0)
        assert user32.GetForegroundWindow() == foreground, "drawing activated the annotation layer"
        responses={}
        for message,expected in ((0x84,-1),(0x21,3)):
            result=ctypes.c_size_t()
            assert user32.SendMessageTimeoutW(overlay["hwnd"],message,0,0,2,1000,ctypes.byref(result))
            actual=ctypes.c_ssize_t(result.value).value
            assert actual == expected,(message,actual,expected)
            responses[str(message)]=actual
        self.evidence["checks"]["native-annotation-hit-test-noactivate-and-foreground-preserved"]={"overlay":overlay,"responses":responses,"foreground":foreground}
        self.stop()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--supplement", action="store_true")
    parser.add_argument("--eof-only", action="store_true")
    parser.add_argument("--overlay-only", action="store_true")
    args = parser.parse_args()
    gate = AnnotationGate(args)
    try:
        gate.start()
        gate.discover_annotation()
        if args.overlay_only:
            gate.overlay_boundary()
        elif args.eof_only:
            gate.eof()
        elif args.supplement:
            gate.supplement()
        else:
            gate.activators()
            gate.raster_ownership()
            gate.replication_policy()
            gate.lifecycle()
            gate.capture()
            gate.stop()
        gate.evidence["result"] = "PASS"
        print(f"PASS: independent annotation native gate. Evidence: {gate.run}", flush=True)
    except Exception as error:
        gate.evidence["result"] = "NOT ACCEPTED"
        gate.evidence["failure"] = repr(error)
        print(f"NOT ACCEPTED: {error!a}. Evidence: {gate.run}", flush=True)
        raise
    finally:
        gate.close()


if __name__ == "__main__":
    main()
