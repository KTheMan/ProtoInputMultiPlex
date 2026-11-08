"""Finite application-cursor acceptance draft. No launch before frozen ROOT READY.

Final source/metadata/fixture binding is reviewed independently before --ready.
Only current Client, owned PrintWindow and existing stopped GUI receipts are used.
"""
import argparse
import copy
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import time


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def freeze(args):
    repo = Path(__file__).resolve().parent.parent
    assert args.ready, "Await final source review/ROOT READY/exclusive slot"
    assert digest(args.producer) == args.producer_sha256.lower()
    p = json.loads(args.producer.read_text(encoding="utf-8"))
    assert p["result"] == "PASS"
    assert p["source_after_sha256"]
    assert all(digest(repo / name) == expected for name, expected in p["source_after_sha256"].items())
    assert digest(args.executable) == p["build_sha256"]["x64"]
    assert digest(args.x86_executable) == p["build_sha256"]["i686"]
    runtime_files = p["runtime_sha256"]
    assert len(runtime_files) == 10
    assert "protoinputmultiplex_cursor_observer64.dll" in runtime_files
    assert "protoinputmultiplex_cursor_observer32.dll" in runtime_files
    assert all(digest(args.runtime / name) == expected for name, expected in runtime_files.items())
    manifest = repo / "runtime/app-cursor-following-legacy-manifest.json"
    assert digest(manifest) == "b049ba471fa501eb5890f6d66d73e822201cb70c1f189c0b0be4baff5928d8ce"
    for name, expected in json.loads(manifest.read_text(encoding="utf-8")).items():
        assert digest(args.legacy_runtime / name) == expected
    if args.phase == "main-tail":
        assert args.from_run and args.from_run.resolve() == repo / "runtime/acceptance-app-cursor-main-20261015-035728"
        raw = args.from_run / "evidence.json"
        review = args.from_run / "independent-prefix-review.json"
        assert digest(raw) == "1d745a4fa5cfc7892f42c0173a825cae431ede609934428185c183a6e9e83baf"
        assert digest(review) == "1bde68abfa15c2c823e7eb4ae8ac5f190853c95b432415248d8374cb9164621f"
        old = json.loads(raw.read_text(encoding="utf-8"))
        assert old["result"] == "NOT ACCEPTED" and old["owned_processes_closed"] and old["known_overlays_closed"]
        assert set(old["originals"]) == {"hand", "ibeam-B", "busy", "resize", "custom", "hidden-label"}
        for name, frame in old["originals"].items():
            assert digest(args.from_run / (name + ".png")) == frame["png_sha256"]
        prior = Path(old["producer"])
        assert digest(prior) == old["producer_sha256"]
        old_sources = json.loads(prior.read_text(encoding="utf-8"))["source_after_sha256"]
        assert set(old_sources) == set(p["source_after_sha256"])
        changed = [name.replace("\\", "/") for name, value in old_sources.items() if p["source_after_sha256"][name] != value]
        assert set(changed) == {"crates/cursor-observer/src/lib.rs",
                                "crates/cursor-snapshot-protocol/src/lib.rs",
                                "crates/mux-windows/src/app_cursor.rs",
                                "crates/protoinputmultiplex/src/automation_probe.rs",
                                "crates/protoinputmultiplex/src/agent_export.rs"}, changed
        for name, value in old["runtime_sha256"].items():
            if not name.startswith("protoinputmultiplex_cursor_observer"):
                assert runtime_files[name] == value
    elif args.phase == "gui-save-tail":
        assert args.from_run and args.from_run.resolve() == repo / "runtime/acceptance-graph-app-cursor-follow-checkbox-20261015-043343"
        assert digest(args.from_run / "evidence.json") == "e145590d32409661eb9d09de3b2ec744490c5ef9688e4f1590912347d9fe83db"
        assert digest(args.from_run / "independent-prefix-review.json") == "6e154313e09e258c2cab258c4926314ead70b2dca7003156368ff6e2a1ac9343"
        old_gui = json.loads((args.from_run / "evidence.json").read_text(encoding="utf-8"))
        assert old_gui["result"] == "NOT ACCEPTED" and len(old_gui["samples"]) == len(old_gui["clicked_events"]) == 6
        assert old_gui["owned_processes_closed"] and old_gui["checkbox_original"]["sha256"] == digest(args.from_run / "follow-checkbox.png")
        assert old_gui["producer_sha256"] == args.producer_sha256.lower()
    else:
        assert args.from_run is None, "Continuation guard belongs only to a specific tail"
    return p


class NativeGate:
    def __init__(self, args, legacy=False):
        self.args, self.legacy = args, legacy
        self.client_module = module("follow_current_mcp", "test-mcp.py")
        self.capture = module("follow_owned_print", "test-cursor-layout-acceptance.py")
        self.capture.user32.SetThreadDpiAwarenessContext(ctypes.c_void_p(-4))
        self.tail = args.phase == "main-tail"
        self.diagnostic = args.phase == "ordinary-diagnostic"
        label = "ordinary-diagnostic" if self.diagnostic else "legacy" if legacy else "main-tail" if self.tail else "main"
        self.folder = args.runtime.resolve() / time.strftime("acceptance-app-cursor-" + label + "-%Y%m%d-%H%M%S")
        self.folder.mkdir()
        self.image = self.folder / "control64.exe"
        shutil.copy2(args.executable, self.image)
        self.probes, self.windows, self.overlays = [], [], set()
        self.client = None
        self.evidence = {"result": "NOT ACCEPTED", "phase": label, "calls": [], "groups": {}, "originals": {},
                         "producer": str(args.producer), "producer_sha256": digest(args.producer),
                         "runtime_directory": str((args.legacy_runtime if legacy else args.runtime).resolve()),
                         "runtime_sha256": (json.loads((Path(__file__).resolve().parent.parent / "runtime/app-cursor-following-legacy-manifest.json").read_text(encoding="utf-8"))
                                            if legacy else json.loads(args.producer.read_text(encoding="utf-8"))["runtime_sha256"]),
                         "x64_sha256": digest(self.image), "i686": "hash verified, not executed"}
        if self.tail:
            self.prefix = json.loads((args.from_run / "evidence.json").read_text(encoding="utf-8"))
            self.evidence["retained_prefix"] = {"path": str(args.from_run), "raw_sha256": digest(args.from_run / "evidence.json"),
                                                  "review_sha256": digest(args.from_run / "independent-prefix-review.json"),
                                                  "scope": "Six prior renderer originals; current observer representative metadata and remaining lifecycle only. Prior whole run remains NOT ACCEPTED."}
        source = self.folder / "runner-source"
        source.mkdir()
        for name in (Path(__file__).name, "test-mcp.py", "test-cursor-layout-acceptance.py"):
            shutil.copy2(Path(__file__).with_name(name), source / name)
        self.save()

    def save(self):
        (self.folder / "evidence.json").write_text(json.dumps(self.evidence, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    def call(self, name, arguments=None):
        entry = {"name": name, "arguments": arguments or {}}
        self.evidence["calls"].append(entry)
        self.save()
        try:
            value = self.client.tool(name, arguments)
        except Exception as error:
            entry["error"] = repr(error)
            self.save()
            raise
        entry["result"] = value
        self.save()
        return value

    def state(self):
        value = self.call("routing_state")
        assert value["running"] and not value.get("last_error"), value
        self.overlays.update(u["overlay"]["hwnd"] for u in value["users"] if u.get("overlay"))
        return value

    def wait(self, op, predicate, timeout=12):
        return self.client_module.eventually(op, predicate, timeout)

    @staticmethod
    def app(user):
        return (user.get("overlay") or {}).get("application_cursor")

    def move(self, user, target, x, y=120):
        w = self.windows[target]
        # The explicit fixture normalizes routed child coordinates back into its
        # own root client. Use that root origin consistently, not an Edit offset.
        bounds = w["client"]
        return self.call("routing_input", {"action": "move_to", "user": user,
                         "x": bounds["left"] + x, "y": bounds["top"] + y})

    def select(self, user, target, x):
        self.move(user, target, x)
        for pressed in (True, False):
            self.call("routing_input", {"action": "button", "user": user, "button": "primary", "pressed": pressed})
        return self.wait(self.state, lambda s: s["users"][user]["pid"] == self.probes[target].pid)

    def settings(self, user, enabled, badge):
        candidate = copy.deepcopy(self.defaults[user])
        candidate.update(follow_application=enabled, label=True, badge=badge, size=32,
                         idle_hide_ms=None, visible=True, lock_window=False)
        result = self.call("routing_settings", {"user": user, "settings": candidate})
        assert next(x["settings"] for x in result["user_settings"] if x["user"] == user) == candidate
        return candidate

    def render(self, user, kind, visible=True, target=0):
        def ready(s):
            u = s["users"][user]
            a = self.app(u)
            return (a and a["capability"] and a["pid"] == u["pid"] == self.probes[target].pid
                    and a["hwnd"] and a["attachment_epoch"] and a["association_epoch"] and a["sequence"]
                    and a["kind"] == kind and a["visible"] is visible and u["overlay"]["paint_generation"] > 0)
        result = self.wait(self.state, ready)
        app = self.app(result["users"][user])
        pid = wintypes.DWORD()
        assert self.capture.user32.GetWindowThreadProcessId(app["hwnd"], ctypes.byref(pid))
        assert pid.value == self.probes[target].pid
        return result

    def frame(self, name, state, user=0):
        from PIL import Image
        wanted = self.app(state["users"][user])
        def painted(current):
            u = current["users"][user]
            o = u["overlay"]
            a = self.app(u)
            return (a and wanted and all(a[k] == wanted[k] for k in ("pid", "kind", "visible", "capability"))
                    and o["paint_generation"] >= state["users"][user]["overlay"]["paint_generation"]
                    and o["rendered_label"] == self.names[user] and not o["label_truncated"]
                    and o["badge"] == ("B" if user else "A") and o["visible"])
        state = self.wait(self.state, painted)
        o = copy.deepcopy(state["users"][user]["overlay"])
        hwnd = o["hwnd"]
        rect = self.capture.Rect()
        assert self.capture.user32.GetWindowRect(hwnd, ctypes.byref(rect))
        assert self.capture.user32.IsWindowVisible(hwnd) and o["visible"]
        assert [rect.left, rect.top] == o["render_origin"]
        assert [rect.right - rect.left, rect.bottom - rect.top] == o["canvas_size"]
        assert [rect.left + o["hotspot"][0], rect.top + o["hotspot"][1]] == [o["x"], o["y"]]
        assert o["rendered_label"] == self.names[user] and not o["label_truncated"]
        assert o["badge"] == ("B" if user else "A") and o["no_activate"] and o["click_through"]
        pixels = self.capture.capture_owned(hwnd, self.folder / (name + ".png"))
        with Image.open(pixels["path"]) as image:
            assert list(image.size) == o["canvas_size"]
        self.evidence["originals"][name] = {"overlay": o, "PrintWindow": pixels,
                                               "png_sha256": digest(pixels["path"]), "verdict": "PENDING DIRECT ORIGINAL REVIEW"}
        self.save()

    def start(self):
        count = 1 if self.legacy or self.diagnostic else 3
        for i in range(count):
            command = [str(self.image), "probe", "--automation"]
            if not self.diagnostic and (self.legacy or i != 2):
                command.append("--cursor-fixture")
            command += ["--report", str(self.folder / f"probe-{i}.json")]
            self.probes.append(subprocess.Popen(command, creationflags=subprocess.CREATE_NO_WINDOW))
        runtime = self.args.legacy_runtime if self.legacy else self.args.runtime
        self.client = self.client_module.Client(self.image, runtime.resolve(), self.folder / "mcp.stderr.log")
        def inventory():
            entries = self.client.tool("routing_windows")["windows"]
            return [next((w for w in entries if w["pid"] == p.pid and w["title"].startswith("ProtoInputMultiPlex Automation Test")), None) for p in self.probes]
        self.windows = self.wait(inventory, lambda v: all(v), 20)
        for i, w in enumerate(self.windows):
            assert self.client_module.user32.SetWindowPos(w["hwnd"], wintypes.HWND(-1), 30 + i * 480, 40, 440, 360, 0x10)
        self.windows = self.wait(inventory, lambda v: all(v))
        self.evidence["owned_targets"] = [{"pid": p.pid, "window": w, "cursor_fixture": not self.diagnostic and (self.legacy or i != 2)}
                                           for i, (p, w) in enumerate(zip(self.probes, self.windows))]
        self.names = ["Ordinary C"] if self.diagnostic else ["Follow A"] if self.legacy else ["Follow A", "Independent B"]
        self.call("routing_start", {"users": self.names, "allowed_pids": [p.pid for p in self.probes],
                                  "mode": "concurrent", "multi_keyboard": True})
        s = self.wait(self.state, lambda s: len(s["users"]) == len(self.names) and all(u["overlay"]["hwnd"] for u in s["users"]))
        assert len({u["overlay"]["hwnd"] for u in s["users"]}) == len(self.names)
        assert all(self.capture.user32.IsWindow(u["overlay"]["hwnd"]) for u in s["users"])
        self.defaults = [next(x["settings"] for x in s["user_settings"] if x["user"] == i) for i in range(len(self.names))]

    def native_reports(self):
        return [json.loads((self.folder / f"probe-{i}.json").read_text(encoding="utf-8")) for i in range(len(self.probes))]

    def main(self):
        self.select(0, 0, 40)
        self.select(1, 1, 120)
        self.settings(0, True, "A")
        b_settings = self.settings(1, True, "B")
        self.move(1, 1, 120)
        b = self.render(1, 32513, target=1)["users"][1]
        self.evidence["groups"]["selected_and_opted_in"] = {"B": b}
        previous = self.state()["users"][0]["overlay"]
        cases = (("custom", 280, 2), ("hidden-label", 280, 0)) if self.tail else (
            ("hand", 40, 32649), ("ibeam-B", 120, 32513), ("busy", 200, 32514),
            ("resize", 360, 32644), ("custom", 280, 2), ("hidden-label", 280, 0))
        metadata = {}
        for name, x, kind in cases:
            self.move(0, 0, x, 240 if kind == 0 else 120)
            self.wait(self.native_reports, lambda rs: rs[0]["cursor_fixture_kind"] == kind)
            s = self.render(0, kind, visible=kind != 0)
            if (previous.get("application_cursor") or {}).get("kind") != kind:
                s = self.wait(self.state, lambda v: self.app(v["users"][0])["kind"] == kind and v["users"][0]["overlay"]["paint_generation"] > previous["paint_generation"])
            if not self.tail:
                self.frame(name, s, user=1 if kind == 32513 else 0)
            else:
                metadata[name] = s
                o = s["users"][0]["overlay"]
                assert o["visible"] and o["rendered_label"] == self.names[0] and o["badge"] == "A"
            if kind == 2:
                image = self.app(s["users"][0])["image"]
                assert [image["width"], image["height"]] == [16, 16] and image["hotspot"] == [3, 5]
                if self.tail:
                    old_image = self.prefix["originals"]["custom"]["overlay"]["application_cursor"]["image"]
                    assert image["identity"] == old_image["identity"], "Unchanged fixture custom pixels changed identity"
            previous = s["users"][0]["overlay"]
            assert s["users"][1]["pid"] == self.probes[1].pid and self.app(s["users"][1])["kind"] == 32513
        self.evidence["groups"]["current_observer_custom_hidden_precedence" if self.tail else "target_stock_custom_hidden"] = (
            metadata if self.tail else list(self.evidence["originals"]))
        self.settings(0, False, "A")
        disabled = self.wait(self.state, lambda s: not next(x["settings"] for x in s["user_settings"] if x["user"] == 0)["follow_application"]
                             and self.app(s["users"][0]) is not None and not self.app(s["users"][0])["sequence"]
                             and self.app(s["users"][0])["visible"] is None
                             and s["users"][0]["overlay"]["paint_generation"] > previous["paint_generation"])
        self.settings(0, True, "A")
        self.move(0, 0, 40)
        previous_a = self.render(0, 32649)
        self.select(0, 2, 120)
        self.move(0, 2, 120)  # Ordinary EDIT class cursor after association; no fixture on C.
        switched = self.render(0, 32513, target=2)
        assert self.app(switched["users"][0])["attachment_epoch"] != self.app(previous_a["users"][0])["attachment_epoch"]
        self.frame("selected-C", switched)
        continued = []
        for _ in range(3):
            time.sleep(.12)
            continued.append(self.state())
        assert all(self.app(s["users"][0])["pid"] == self.probes[2].pid and self.app(s["users"][0])["kind"] == 32513 for s in continued)
        assert all(s["users"][1]["pid"] == self.probes[1].pid and self.app(s["users"][1])["kind"] == 32513
                   and next(x["settings"] for x in s["user_settings"] if x["user"] == 1) == b_settings for s in continued)
        self.evidence["groups"]["disable_switch_independent"] = {"disabled": disabled, "switched": switched, "continued": continued}
        self.close_probe(2)
        cleared = self.wait(self.state, lambda s: s["users"][0]["pid"] == 0 and (not self.app(s["users"][0]) or not self.app(s["users"][0]).get("sequence")))
        self.frame("selected-exit-fallback", cleared)
        self.call("routing_input", {"action": "key", "user": 1, "key": "f7", "pressed": True})
        self.wait(self.native_reports, lambda rs: 118 in rs[1]["held_keys"])
        stopped = self.call("routing_stop")
        assert not stopped["running"]
        reports = self.wait(self.native_reports, lambda rs: not rs[1]["held_keys"] and len(rs[1]["key_events"]) == 2)
        assert [(x["vk"], x["pressed"]) for x in reports[1]["key_events"]] == [(118, True), (118, False)]
        assert reports[1]["key_events"][-1]["scan"] == 65 and not reports[1]["key_events"][-1]["extended"] and not reports[1]["key_events"][-1]["alt"]
        self.wait(lambda: all(not self.capture.user32.IsWindow(h) for h in self.overlays), bool)
        self.evidence["groups"]["exit_stop"] = {"cleared": cleared, "native_reports": reports, "stopped": stopped}
        self.call("routing_start", {"users": self.names, "allowed_pids": [self.probes[1].pid], "mode": "concurrent", "multi_keyboard": True})
        self.wait(self.state, lambda s: len(s["users"]) == 2)
        self.settings(0, True, "A")
        fresh = self.wait(self.state, lambda s: self.app(s["users"][0]) is not None
                          and self.app(s["users"][0])["capability"] and not self.app(s["users"][0])["sequence"]
                          and self.app(s["users"][0])["image"] is None and self.app(s["users"][0])["pid"] == 0)
        self.call("routing_stop")
        self.evidence["groups"]["exit_stop"]["fresh_session_no_old_snapshot"] = fresh

    def ordinary_diagnostic(self):
        # One ordinary EDIT target, no cursor fixture and no images. Stop is the
        # sixth mutation even when following never converges; state reads only.
        self.settings(0, True, "C")
        selected = self.select(0, 0, 120)
        self.evidence["selection_ack"] = selected
        observations = self.evidence["ordinary_observations"] = []
        deadline = time.monotonic() + 12
        following = False
        while True:
            state = self.state()
            user = state["users"][0]
            app = self.app(user)
            assert app is not None and "provider_status" in app, "Diagnostic requires explicit coherent provider_status metadata"
            observations.append({"at_unix": time.time(), "selected_pid": user["pid"],
                                 "application_cursor": copy.deepcopy(app)})
            assert user["pid"] == self.probes[0].pid, "Ordinary selected target changed"
            if app["hwnd"]:
                owner = wintypes.DWORD()
                assert self.capture.user32.GetWindowThreadProcessId(app["hwnd"], ctypes.byref(owner))
                assert owner.value == self.probes[0].pid
            following = (app["capability"] and app["pid"] == self.probes[0].pid
                         and app["hwnd"] and app["sequence"] and app["provider_status"] == 1
                         and app["kind"] == 32513 and app["visible"] is True)
            self.save()
            if following or time.monotonic() >= deadline:
                break
            time.sleep(.12)
        self.evidence["ordinary_outcome"] = "IBeam following observed" if following else "IBeam following not observed within deadline"
        self.evidence["ordinary_final_state"] = state
        reports = self.wait(self.native_reports, lambda rs: len(rs[0]["mouse_events"]) >= 2
                            and rs[0].get("cursor_selection_count", 0) > 0
                            and any(event["hwnd"] == app["hwnd"] for event in rs[0].get("cursor_selection_events", [])))
        trace = reports[0]["cursor_selection_events"]
        assert len(trace) == min(16, reports[0]["cursor_selection_count"])
        for event in trace:
            assert set(event) == {"hwnd", "thread_id", "hit_test", "trigger", "native_result",
                                  "before_kind", "after_kind", "after_resource"}
            assert event["hwnd"] and event["thread_id"]
            owner = wintypes.DWORD()
            assert self.capture.user32.GetWindowThreadProcessId(event["hwnd"], ctypes.byref(owner)) == event["thread_id"]
            assert owner.value == self.probes[0].pid
            resource = event["after_resource"]
            assert set(resource) == {"success", "resource_id", "module", "resource"}
            assert len(resource["module"].encode("utf-16-le")) <= 512 and len(resource["resource"].encode("utf-16-le")) <= 512
        self.evidence["ordinary_handler_trace"] = copy.deepcopy(trace)
        assert [(x["button"], x["pressed"]) for x in reports[0]["mouse_events"]] == [(0, True), (0, False)]
        assert reports[0]["cursor_fixture_kind"] is None and not reports[0]["buttons"] and not reports[0]["held_keys"]
        self.evidence["ordinary_native_before_stop"] = reports
        stopped = self.call("routing_stop")
        assert not stopped["running"]
        self.wait(lambda: all(not self.capture.user32.IsWindow(h) for h in self.overlays), bool)
        self.evidence["ordinary_stop"] = stopped
        self.evidence["ordinary_native_after_stop"] = self.native_reports()

    def compatibility(self):
        self.settings(0, True, "A")
        self.select(0, 0, 40)
        s = self.wait(self.state, lambda s: self.app(s["users"][0]) is not None and self.app(s["users"][0])["capability"] is False)
        assert next(x["settings"] for x in s["user_settings"] if x["user"] == 0)["follow_application"]
        self.frame("legacy-unavailable-fallback", s)
        stopped = self.call("routing_stop")
        assert not stopped["running"]
        self.evidence["groups"]["missing_optional_capability"] = {"state": s, "stop": stopped}

    def close_probe(self, i):
        p = self.probes[i]
        if p.poll() is None:
            u = self.client_module.user32
            u.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, ctypes.c_size_t, ctypes.c_ssize_t]
            assert u.PostMessageW(self.windows[i]["hwnd"], 0x10, 0, 0)
            p.wait(timeout=8)
        assert p.returncode == 0

    def run(self):
        try:
            self.start()
            self.ordinary_diagnostic() if self.diagnostic else self.compatibility() if self.legacy else self.main()
            count = sum(c["name"] != "routing_state" for c in self.evidence["calls"])
            assert count <= (6 if self.legacy or self.diagnostic else 32)
            if self.diagnostic:
                assert count == 6 and not self.evidence["originals"]
                self.evidence.update(result="DIAGNOSTIC COMPLETE — NO FEATURE ACCEPTANCE", mutation_calls=count)
            else:
                self.evidence.update(result="PENDING INDEPENDENT DATA/PIXEL REVIEW", data_result="PASS", mutation_calls=count)
        except Exception as error:
            self.evidence.update(result="NOT ACCEPTED", failure=repr(error))
            raise
        finally:
            errors = []
            if self.client:
                try:
                    self.client.close()
                except Exception as error:
                    errors.append(repr(error))
                    if self.client.process.poll() is None:
                        self.client.process.kill()
                        self.client.process.wait(timeout=12)
                    self.client.stderr.close()
            for i, p in enumerate(self.probes):
                if p.poll() is None:
                    try:
                        self.close_probe(i)
                    except Exception as error:
                        errors.append(repr(error))
                        if p.poll() is None:
                            p.terminate()
                            p.wait(timeout=10)
            closed = all(p.poll() is not None for p in self.probes) and (self.client is None or self.client.process.poll() is not None)
            self.evidence.update(cleanup_errors=errors, owned_processes_closed=closed,
                                 known_overlays_closed=all(not self.capture.user32.IsWindow(h) for h in self.overlays))
            if errors or not closed or not self.evidence["known_overlays_closed"]:
                self.evidence["result"] = "NOT ACCEPTED"
            self.save()
            print(self.evidence["result"], self.folder, flush=True)


def gui(args):
    lifecycle = module("follow_stopped_gui", "test-app-lifecycle-ui.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    class Gate(Base):
        def __init__(self):
            super().__init__(args, "app-cursor-follow-save-tail" if args.phase == "gui-save-tail" else "app-cursor-follow-checkbox", offline=True, count=0)
            self.frame_request = None
        def frame(self):
            hwnd = self.main_hwnd()
            if self.frame_request is None:
                geometry.resize_owned_client(hwnd, [540, 520])
                self.frame_request = time.time()
                return False
            shot = geometry.copy_owned_product_frame(self.run / "ui.png", self.run / "follow-checkbox.png", hwnd, self.frame_request)
            if not shot:
                return False
            self.evidence["checkbox_original"] = shot
            return True
    g = Gate()
    if args.phase == "gui-save-tail":
        g.evidence["retained_checkbox_original"] = {"path": str(args.from_run / "follow-checkbox.png"),
                                                     "sha256": digest(args.from_run / "follow-checkbox.png"),
                                                     "scope": "Same frozen build prior actual checkbox pixels; no new capture in Save tail"}
    g.evidence.update(producer=str(args.producer), producer_sha256=digest(args.producer),
                      scope="Stopped automatic-profile checkbox and actual Save only; no worker or native DLL use")
    pids = [987501, 987502]
    profile = {"version": 1, "concurrent": True, "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": pids},
               "seats": [{"id": "follow-gui-" + str(i), "name": "Follow GUI " + str(i + 1),
                          "mouse": f"virtual://mouse/{i+1}", "keyboard": f"virtual://keyboard/{i+1}",
                          "target_pid": 0, "cursor": {"follow_application": False}} for i in range(2)]}
    prefs = {"version": 1, "appearance_mode": "dark", "zoom_percent": 100, "launch_minimized": False,
             "minimize_to_tray": False, "close_to_tray": False, "launch_at_login": False, "shortcuts": []}
    labels = [("select-B", "Select user 2"), ("configure", "Configure user"), ("theme", "Theme"),
              ("follow", "Follow application cursor"), ("home", "Back to Home"), ("settings", "Settings"),
              ("profiles", "Profiles & backend"), ("save", "Save profile")]
    if args.phase == "gui-save-tail":
        labels.insert(6, ("users-category", "Settings category Users & devices"))
    expected = None
    def verify(name, sample):
        nonlocal expected
        actual = g.snapshot()["profile"]
        if expected is None:
            expected = copy.deepcopy(actual)
        wanted = copy.deepcopy(expected)
        if name in {"follow", "home", "settings", "users-category", "profiles", "save"}:
            wanted["seats"][1]["cursor"]["follow_application"] = True
            if actual == expected:
                return False  # Previous accepted snapshot only; never mutate expectation on retry.
        assert actual == wanted, "Checkbox/navigation changed unrelated normalized profile"
        assert ui.read_json(g.preferences_path) == prefs
        if name == "follow" and args.phase != "gui-save-tail" and not g.frame():
            return False
        if name == "save":
            try:
                saved = ui.read_json(g.run / "profile.json")
            except (OSError, ValueError):
                return False
            if saved in (profile, expected):
                return False
            assert saved == wanted
            g.evidence["saved_profile"] = saved
        sample["complete_profile_checked"] = True
        return True
    def after(gate):
        assert not (gate.run / "ui.state.json").exists(), "Stopped checkbox gate created a worker"
        gate.expected_exit = True
        assert geometry.user32.PostMessageW(gate.main_hwnd(), 0x10, 0, 0)
        gate.gui.wait(timeout=10)
        assert gate.gui.returncode == 0 and not hotkeys.lifecycle_window(gate.gui.pid)
        return True
    try:
        provenance = g.run / "runner-source"
        provenance.mkdir()
        shutil.copy2(Path(__file__), provenance / Path(__file__).name)
        g.execute(profile, prefs, [ui.step(name, label) for name, label in labels], verify,
                  after_controls=after)
        g.evidence.update(result="PENDING INDEPENDENT DATA/PIXEL REVIEW", control_profile_result="PASS")
        g.save()
    finally:
        g.close()
        lifecycle.remove_startup_key(g.startup_key)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready", action="store_true")
    parser.add_argument("--phase", choices=("main", "main-tail", "ordinary-diagnostic", "legacy", "gui", "gui-save-tail"), required=True)
    parser.add_argument("--from-run", type=Path)
    for name in ("executable", "x86-executable", "runtime", "legacy-runtime", "producer"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--producer-sha256", required=True)
    args = parser.parse_args()
    freeze(args)
    gui(args) if args.phase in ("gui", "gui-save-tail") else NativeGate(args, args.phase == "legacy").run()


if __name__ == "__main__":
    main()
