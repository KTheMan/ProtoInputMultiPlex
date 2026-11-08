"""Finite keyboard source/recipient integration gate. Explicit ROOT READY required."""
import argparse
import copy
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec); spec.loader.exec_module(result); return result


def digest(path): return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def freeze(args):
    assert args.ready, "Await source approval, frozen producer and ROOT READY"
    assert digest(args.producer) == args.producer_sha256.lower()
    producer = json.loads(args.producer.read_text(encoding="utf-8"))
    assert producer["result"] == "PASS" and len(producer["checks"]) == 5
    assert all(c["exit_code"] == 0 for c in producer["checks"])
    assert producer["source_before_sha256"] == producer["source_after_sha256"]
    repo = Path(__file__).resolve().parent.parent
    prior = repo/"runtime/acceptance-graph-keyboard-delivery-20261015-085740"
    for filename, expected in {"evidence.json":"3532b4913ccd48a643f8212774e5c1e0967afb3f2f1f9bbeacf0811bb8220d07","independent-review.json":"3d6ecfd7aa4a4f9316d2a76ff05caeb4ef945b168b049d16676d642f6a5bb74f","post-terminal-cleanup.json":"38abd83735464701fe37b799f6a50c7ffcd447d6e88e9bd22c4574cb1d681746"}.items(): assert digest(prior/filename) == expected
    old = json.loads((repo/"runtime/producer-keyboard-delivery-20261015-085346/verification.json").read_text(encoding="utf-8"))
    assert set(old["source_after_sha256"]) == set(producer["source_after_sha256"])
    changed = {n.replace('\\','/') for n,h in old["source_after_sha256"].items() if producer["source_after_sha256"][n] != h}
    assert changed == {"crates/protoinputmultiplex/src/automation_probe.rs"}, changed
    assert old["runtime_sha256"] == producer["runtime_sha256"]
    assert all(digest(repo / n) == h for n, h in producer["source_after_sha256"].items())
    assert digest(args.executable) == producer["build_sha256"]["x64"]
    assert digest(args.x86_executable) == producer["build_sha256"]["i686"]
    assert all(digest(args.runtime / n) == h for n, h in producer["runtime_sha256"].items())
    return producer


def workflow(args, producer):
    lifecycle = module("keyboard_delivery_base", "test-app-lifecycle-ui.py")
    connected = module("keyboard_delivery_mcp", "test-connected-agents-acceptance.py")
    strict = module("keyboard_delivery_strict", "test-keyboard-persistence-ui.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    geometry.user32.GetParent.argtypes = [wintypes.HWND]; geometry.user32.GetParent.restype = wintypes.HWND
    geometry.user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
    ids = ["keyboard-owned-a", "keyboard-owned-b"]

    class Gate(Base):
        def __init__(self):
            super().__init__(args, "keyboard-delivery", offline=False, count=2)
            self.targets = []; self.roots = []; self.client = None; self.descriptor = None
            self.layers = set(); self.calls = []; self.mutations = 0; self.done = set()
            self.frames = {}; self.holds = {}; self.expected = None; self.expected_keyboard = {"enabled": True, "focus": "clicked"}
        def snapshot(self): return ui.eventually(lambda: super(Gate, self).snapshot(), lambda v: True, 3)
        def probes(self): return ui.eventually(lambda: [ui.read_json(self.run / f"probe-{i}.json") for i in range(2)], lambda v: True, 3)
        def once(self, name, fn):
            if name in self.done: return
            # Imperative callbacks must never be retried by Base's transient JSON catch.
            try: fn()
            except (OSError, ValueError) as error: raise RuntimeError("Never replay imperative phase " + name) from error
            self.done.add(name); self.save()
        def fixture(self):
            for i in range(2):
                proc = subprocess.Popen([str(self.images[0]), "probe", "--automation", "--keyboard-focus-fixture", "--report", str(self.run / f"probe-{i}.json")], creationflags=subprocess.CREATE_NO_WINDOW)
                self.targets.append(proc); self.owned.append(proc)
                hwnd = ui.eventually(lambda: geometry.owned_window(proc.pid), bool, 10)
                self.roots.append(int(hwnd))
                assert geometry.user32.SetWindowPos(hwnd, wintypes.HWND(-1), 40 + 460*i, 50, 420, 360, 0x10)
            self.pids = [p.pid for p in self.targets]
            reports = ui.eventually(self.probes, lambda rs: all(r["frames"] > 2 and r["keyboard_focus_fixture"] for r in rs), 10)
            for i, r in enumerate(reports):
                f = r["keyboard_focus_fixture"]; assert f["second_edit_focus_policy"] == "preserve_initial_left_focus"
                assert f["root"] == self.roots[i] and len(set(f["edits"])) == 2
                assert f["seeded"]["available"] and f["seeded"]["focused"] == f["edits"][0] == f["seeded"]["caret"]
                assert f["foreground_before_seed"] == f["foreground_after_seed"]
                for h in [f["root"], *f["edits"]]:
                    pid = wintypes.DWORD(); tid = geometry.user32.GetWindowThreadProcessId(h, ctypes.byref(pid))
                    assert tid == f["thread_id"] and pid.value == self.pids[i] and ui.user32.IsWindow(h)
                for child in f["edits"]:
                    label = ctypes.create_unicode_buffer(32)
                    assert geometry.user32.GetParent(child) == f["root"] and geometry.user32.GetClassNameW(child, label, len(label)) and label.value.casefold() == "edit"
            self.evidence["pre_injection_native_focus"] = reports
        def call(self, name, arguments=None):
            read_only = name == "routing_state" or (name == "macro_studio" and arguments.get("operation") == "inspect")
            if not read_only: self.mutations += 1
            assert self.mutations <= 28
            record = {"tool": name, "arguments": arguments or {}, "mutating": not read_only, "started_ns": time.monotonic_ns()}
            self.calls.append(record); self.evidence["mcp_calls"] = self.calls; self.save()
            try: record["result"] = self.client.tool(name, arguments)
            except (OSError, ValueError) as error: raise RuntimeError("MCP call cannot be resubmitted") from error
            record["finished_ns"] = time.monotonic_ns(); self.save(); return record["result"]
        def state(self):
            value = self.call("routing_state")
            if hasattr(self, "client_id"): assert value["agent_session"]["client"] == self.client_id
            return value
        def input(self, action, **fields): return self.call("routing_input", {"action": action, **fields})
        def key(self, user, key, pressed): return self.input("key", user=user, key=key, pressed=pressed)
        def native(self, predicate): return ui.eventually(self.probes, predicate, 8)
        def key_events(self, report, vk): return [e for e in report["native_events"] if e["kind"] == "key" and e["code"] == vk]
        def keyboard_events(self, report): return [e for e in report["native_events"] if e["kind"] in ("key", "char")]
        def wait_settings(self, settings):
            return ui.eventually(self.state, lambda s: s["running"] and s["users"][0]["keyboard"]["settings"] == settings, 8)
        def check_profile(self, profile, keyboard):
            expected = copy.deepcopy(self.expected); expected["seats"][0]["cursor"]["keyboard"] = keyboard
            strict.strict_equal(profile, expected)
        def connect(self):
            paths = list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{self.gui.pid}-*.json")); assert len(paths) == 1
            self.descriptor = paths[0]; self.client = connected.ConnectedMcp(self.images[0], self.descriptor, self.run / "mcp.stderr.log")
            self.owned.append(self.client.process)
            value = self.state(); self.client_id = value["agent_session"]["client"]; assert self.client_id > 0
            assert set(value["agent_session"]["policy"]["user_ids"]) == set(ids)
            assert set(value["agent_session"]["policy"]["allowed_pids"]) == set(self.pids) and value["agent_session"]["policy"]["session_control"]
            self.expected = copy.deepcopy(self.snapshot()["profile"])
            self.layers.update(u["overlay"]["hwnd"] for u in value["users"]); assert len(self.layers) == 2 and all(h and ui.user32.IsWindow(h) for h in self.layers)
            for i, report in enumerate(self.probes()):
                child = report["keyboard_focus_fixture"]["edits"][1]; rect = geometry.Rect()
                assert geometry.user32.GetWindowRect(child, ctypes.byref(rect))
                self.input("move_to", user=i, x=rect.left+70, y=rect.top+60)
                for pressed in (True, False): self.input("button", user=i, button="primary", pressed=pressed)
            value = ui.eventually(self.state, lambda s: all(s["users"][i]["keyboard"]["clicked"] and s["users"][i]["keyboard"]["clicked"]["hwnd"] == self.probes()[i]["keyboard_focus_fixture"]["edits"][1] for i in range(2)), 8)
            clocks = [r["clock_us"] for r in self.probes()]
            stable = self.native(lambda rs: all(r["clock_us"] >= clocks[i]+300000 for i,r in enumerate(rs)))
            for r in stable:
                f = r["keyboard_focus_fixture"]
                assert f["current"]["available"] and f["current"]["focused"] == f["edits"][0] == f["current"]["caret"], f
            self.evidence["post_click_real_focus_prerequisite"] = stable
            self.evidence["initial_selected"] = value
        def pair(self, user, key, vk, required_hwnd=None):
            before = self.probes(); row = self.state()["users"][user]["keyboard"]
            destination = row["resolved"]["hwnd"]; assert destination and (required_hwnd is None or destination == required_hwnd)
            count = len(self.key_events(before[user], vk))
            for pressed in (True, False): self.key(user, key, pressed)
            reports = self.native(lambda rs: len(self.key_events(rs[user], vk)) >= count+2)
            delta = self.key_events(reports[user], vk)[count:]
            assert [(e["pressed"], e["hwnd"]) for e in delta] == [(True, destination), (False, destination)], delta
            self.evidence.setdefault("recipient_pairs", {})[key+"-"+str(user)] = {"candidate": row, "native": delta, "real_focus": reports[user]["keyboard_focus_fixture"]["current"]}
        def focused(self):
            reports = self.probes(); left = reports[0]["keyboard_focus_fixture"]["edits"][0]
            row = self.state()["users"][0]["keyboard"]; assert row["resolved"]["hwnd"] == left and not row["fallback"]
            before = reports[0]["keyboard_focus_fixture"]["edit_text"]
            self.input("text", user=0, text="focus-λ")
            reports = self.native(lambda rs: rs[0]["keyboard_focus_fixture"]["edit_text"][0] == before[0]+"focus-λ")
            assert reports[0]["keyboard_focus_fixture"]["edit_text"][1] == before[1]
            self.f5_before = [len(self.key_events(r, 116)) for r in reports]
            self.key(0, "f5", True)
            self.native(lambda rs: len(self.key_events(rs[0], 116)) == self.f5_before[0]+1)
            assert self.key_events(self.probes()[0], 116)[-1]["hwnd"] == left
            self.evidence["focused_text_and_held_original"] = {"left": left, "reports": self.probes()}
        def original_release_and_clicked(self):
            reports = self.native(lambda rs: all(len(self.key_events(rs[i], 116)) >= self.f5_before[i]+2 for i in range(2)))
            for i, r in enumerate(reports):
                wanted = r["keyboard_focus_fixture"]["edits"][0 if i == 0 else 1]
                delta = self.key_events(r, 116)[self.f5_before[i]:]
                assert [(e["pressed"], e["hwnd"]) for e in delta] == [(True, wanted), (False, wanted)]
            self.evidence["settings_releases_original_recipient"] = reports
            self.pair(0, "f6", 117, reports[0]["keyboard_focus_fixture"]["edits"][1])
        def studio(self, operation, **fields):
            reply = self.call("macro_studio", {"operation": operation, "user": 0, **fields})
            state = reply["macro_studio"]
            if operation != "inspect": assert state["acknowledged"] and not state.get("error")
            return next(r for r in state["users"] if r["user"] == 0)
        def disabled(self):
            before = self.probes(); signature = [self.keyboard_events(r) for r in before]
            lane = self.studio("record", active=True); assert lane["recording"] and lane["recorded_events"] == 0
            self.key(0, "escape", True); self.key(0, "escape", False); self.input("text", user=0, text="blocked-λ")
            clocks = [r["clock_us"] for r in self.probes()]
            after = self.native(lambda rs: all(r["clock_us"] >= clocks[i]+300000 for i, r in enumerate(rs)))
            assert [self.keyboard_events(r) for r in after] == signature
            assert [r["keyboard_focus_fixture"]["edit_text"] for r in after] == [r["keyboard_focus_fixture"]["edit_text"] for r in before]
            lane = self.studio("inspect"); assert lane["recording"] and lane["recorded_events"] == 0
            self.evidence["disabled_raw_excluded"] = {"before": before, "after": after, "lane": lane, "state": self.state()}
            count = len(self.key_events(after[0], 113)); row = self.state()["users"][0]["keyboard"]
            for pressed in (True, False): self.input("button", user=0, button="back", pressed=pressed)
            after = self.native(lambda rs: len(self.key_events(rs[0], 113)) >= count+2)
            assert [(e["pressed"], e["hwnd"]) for e in self.key_events(after[0], 113)[count:]] == [(True, row["resolved"]["hwnd"]), (False, row["resolved"]["hwnd"])]
            assert self.key_events(after[1], 113) == self.key_events(before[1], 113)
            lane = self.studio("inspect"); assert lane["recording"] and lane["recorded_events"] == 2
            lane = self.studio("record", active=False); assert not lane["recording"] and lane["loaded_events"] == 2
            actions = [e["action"] for e in lane["clip"]["events"]]
            assert all(a["type"] == "packet" and a["packet"]["kind"] == 0 for a in actions) and [a["packet"]["flags"] for a in actions] == [64, 128], actions
            self.evidence["mouse_conversion_retained_recorded_raw"] = {"lane": lane, "native": after}
            self.pair(1, "f3", 114, before[1]["keyboard_focus_fixture"]["edits"][1])
        def external(self, focus):
            candidate = copy.deepcopy(self.expected["seats"][0]["cursor"]); candidate["keyboard"] = {"enabled": True, "focus": focus}
            reply = self.call("routing_settings", {"user": 0, "settings": candidate})
            state = self.wait_settings(candidate["keyboard"])
            self.evidence.setdefault("external_settings", {})[focus] = {"reply": reply, "state": state}
        def frame(self, name, size):
            if name in self.frames: return True
            hwnd = self.main_hwnd(); actual = geometry.geometry(hwnd)
            if actual["client_logical"] != [float(v) for v in size]: geometry.resize_owned_client(hwnd, size); return False
            hold = self.holds.setdefault(name, {"start": time.time(), "writes": []}); grace = hold["start"]+3.2
            if time.time() < grace: return False
            try:
                from PIL import Image
                path = self.run / "ui.png"; stat = path.stat()
                if stat.st_mtime <= grace: return False
                raw = path.read_bytes(); picture = Image.open(io.BytesIO(raw)); picture.load()
                if picture.size != tuple(actual["client_pixels"]) or path.stat().st_mtime_ns != stat.st_mtime_ns: return False
                item = [stat.st_mtime_ns, len(raw), hashlib.sha256(raw).hexdigest()]
                if not hold["writes"] or item[0] != hold["writes"][-1][0]: hold["writes"].append(item)
                if len(hold["writes"]) < 2: return False
                saved = geometry.copy_owned_product_frame(path, self.run/(name+".png"), hwnd, grace)
                if not saved: return False
                self.frames[name] = saved; self.evidence["originals"] = self.frames; self.evidence["capture_holds"] = self.holds; self.save(); return True
            except (OSError, ValueError): return False
        def close_targets(self):
            for proc, hwnd in zip(self.targets, self.roots):
                if proc.poll() is None:
                    assert geometry.user32.PostMessageW(hwnd, 0x10, 0, 0); proc.wait(timeout=8)
                assert proc.returncode == 0

    g = Gate()
    prefs = {"version":1,"appearance_mode":"dark","zoom_percent":100,"launch_minimized":False,"minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,"shortcuts":[]}
    try:
        g.fixture()
        output = {"buttons":[{"type":"legacy"} for _ in range(5)],"keys":{}}; output["buttons"][3] = {"type":"key","key":113}
        profile = {"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":g.pids},"replication":{"version":1,"edges":[{"source":ids[0],"target":ids[1],"mouse":False,"keyboard":True,"text":True}]},"seats":[{"id":ids[i],"name":"Keyboard "+("Alpha" if i==0 else "Beta"),"mouse":f"virtual://mouse/{i+1}","keyboard":f"virtual://keyboard/{i+1}","desktop_start":[60+460*i,110],"cursor":{"lock_window":False,"keyboard":{"enabled":True,"focus":"clicked"}},"output":output if i==0 else {"buttons":[{"type":"legacy"} for _ in range(5)],"keys":{}}} for i in range(2)]}
        steps = [ui.step("start","Start routing"),ui.step("settings","Settings"),ui.step("agent-category","Settings category Agent access"),ui.step("agent","Agent Access"),ui.step("enable","Enable agent access",{"agent_users":','.join(ids),"agent_pids":','.join(map(str,g.pids)),"agent_session_control":"true","agent_capture":"false","agent_app_preferences":"false"}),ui.step("home-configure","Back to Home"),ui.step("select-a","Select user 1"),ui.step("configure","Configure user"),ui.step("keyboard","Keyboard")]
        for key, label in [("focused","Focused control"),("clicked","Clicked control"),("caret","Caret window"),("native","Direct focus lookup"),("active","Active application window")]:
            steps += [ui.step("draft-"+key,label),ui.step("apply-"+key,"Apply cursor settings")]
        steps += [ui.step("disable","Keyboard enabled"),ui.step("apply-disabled","Apply cursor settings"),ui.step("dirty-clicked","Clicked control"),ui.step("home-stop","Back to Home"),ui.step("stop","Stop routing")]
        assert len(steps) == 24
        def verify(name, sample):
            strict.strict_equal(ui.read_json(g.preferences_path), prefs)
            if name == "enable": g.once("connect", g.connect)
            if g.expected is None: return True
            if name.startswith("draft-"): g.expected_keyboard = {"enabled":True,"focus":name.removeprefix("draft-")}
            elif name == "disable": g.expected_keyboard = {"enabled":False,"focus":"active"}
            elif name == "dirty-clicked": g.expected_keyboard = {"enabled":True,"focus":"clicked"}
            candidate = sample["profile_state"]["profile"]
            if name == "apply-disabled" and "external-focused" in g.done and candidate["seats"][0]["cursor"]["keyboard"] == {"enabled":False,"focus":"active"}:
                g.check_profile(candidate,{"enabled":False,"focus":"active"}); return False
            g.check_profile(candidate, g.expected_keyboard)
            if name.startswith("apply-") and name != "apply-disabled":
                focus = name.removeprefix("apply-"); settings = {"enabled":True,"focus":focus}
                state = g.wait_settings(settings)
                g.evidence.setdefault("gui_apply_states", {})[focus] = state
                if focus == "focused":
                    g.once(name, g.focused)
                    if not g.frame("keyboard-focused-normal", [540,520]): return False
                elif focus == "clicked": g.once(name, g.original_release_and_clicked)
                else:
                    number = {"caret":(118,"f7"),"native":(119,"f8"),"active":(120,"f9")}[focus]
                    required = g.probes()[0]["keyboard_focus_fixture"]["edits"][0] if focus in ("caret","native") else None
                    g.once(name, lambda:g.pair(0,number[1],number[0],required))
            if name == "apply-disabled":
                if "external-focused" not in g.done:
                    g.wait_settings({"enabled":False,"focus":"active"})
                    if not g.frame("keyboard-disabled-minimum", [320,360]): return False
                    g.once("disabled", g.disabled)
                    g.once("external-focused", lambda:g.external("focused"))
                    g.expected_keyboard = {"enabled":True,"focus":"focused"}
                current = g.snapshot()["profile"]
                if current["seats"][0]["cursor"]["keyboard"] != g.expected_keyboard: return False
                g.check_profile(current,g.expected_keyboard)
            if name == "dirty-clicked":
                g.once("external-native",lambda:g.external("native"))
                g.wait_settings({"enabled":True,"focus":"native"})
                g.check_profile(g.snapshot()["profile"],{"enabled":True,"focus":"clicked"})
                if not g.frame("keyboard-external-ack-preserved-draft",[540,520]): return False
            if name == "stop":
                if any(ui.user32.IsWindow(h) for h in g.layers) or g.descriptor.exists(): return False
                recovery = ui.read_json(g.run/"ui.recovery.json")
                if recovery["phase"] != "stopped": return False
                assert recovery["can_start"] and all(not r["held_keys"] and not r["buttons"] for r in g.probes())
                assert g.mutations == 27, g.mutations
                g.evidence["stop_recovery"] = recovery
            return True
        def after(gate):
            if gate.client: gate.client.close(); gate.client = None
            gate.close_targets(); gate.expected_exit = True
            assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0); gate.gui.wait(timeout=10)
            assert gate.gui.returncode == 0 and not hotkeys.lifecycle_window(gate.gui.pid)
            assert not gate.descriptor.exists()
            return True
        archive = g.run/"runner-source"; archive.mkdir()
        for name in (Path(__file__).name,"test-app-lifecycle-ui.py","test-profile-maintenance.py","test-replication-graph-ui.py","test-horizontal-wheel-ui.py","test-connected-agents-acceptance.py","owned-gui-geometry.py","test-keyboard-persistence-ui.py"):
            shutil.copy2(Path(__file__).with_name(name),archive/name)
        ui.put(archive/"all-script-dependency-hashes.json",{f.name:digest(f) for f in Path(__file__).parent.glob("*.py")})
        g.evidence.update(producer=str(args.producer),producer_sha256=digest(args.producer),source_sha256=producer["source_after_sha256"],runtime_sha256=producer["runtime_sha256"],owned_probe_pids=g.pids,scope="Keyboard source and application-scoped recipients; 24 GUI/27 expected mutating MCP; no hardware/full parity")
        g.execute(profile,prefs,steps,verify,after_controls=after)
        g.evidence.update(result="PENDING INDEPENDENT DATA/PIXEL REVIEW",mcp_mutations=g.mutations)
    finally:
        if g.client:
            try:g.client.close()
            except Exception as error:
                g.evidence.setdefault("cleanup_errors",[]).append(repr(error))
                if g.client.process.poll() is None:g.client.process.kill();g.client.process.wait(timeout=8)
                g.client.stderr.close()
        try:g.close_targets()
        except Exception as error:g.evidence.setdefault("cleanup_errors",[]).append(repr(error))
        g.close(); lifecycle.remove_startup_key(g.startup_key)
        g.evidence["known_overlays_closed"] = bool(g.layers) and all(not ui.user32.IsWindow(h) for h in g.layers)
        g.evidence["owned_exit_codes"] = {str(p.pid):p.returncode for p in g.owned}
        g.evidence["descriptor_absent"] = g.descriptor is None or not g.descriptor.exists()
        g.evidence["guid_value_absent"] = lifecycle.startup_value(g.startup_key) is None
        pids = ','.join(str(p.pid) for p in g.owned)
        audit = subprocess.run(["powershell","-NoProfile","-Command",f"@(Get-CimInstance Win32_Process | Where-Object {{ $_.ProcessId -in @({pids}) }} | Select-Object ProcessId,Name,ExecutablePath) | ConvertTo-Json -Compress"],capture_output=True,text=True,timeout=20)
        assert audit.returncode == 0, audit.stderr
        survivors = json.loads(audit.stdout) if audit.stdout.strip() else []
        g.evidence["scoped_cim_survivors"] = survivors
        if survivors or not g.evidence["guid_value_absent"]:g.evidence["result"]="NOT ACCEPTED"
        if g.evidence.get("cleanup_errors") or not all(p.poll() is not None for p in g.owned):g.evidence["result"]="NOT ACCEPTED"
        g.save()


def main():
    parser=argparse.ArgumentParser();parser.add_argument("--ready",action="store_true")
    for name in ("executable","x86-executable","runtime","producer"):parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--producer-sha256",required=True)
    args=parser.parse_args();workflow(args,freeze(args))


if __name__ == "__main__":main()
