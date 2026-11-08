"""Bounded automatic-desktop target-exit and explicit recovery gate.

Launch only after the repository owner marks this exact runner READY and grants
the native slot. Every target, client, window, and GUI process is owned here.
"""
import argparse
import ctypes
from ctypes import wintypes
import importlib.util
import json
import shutil
from pathlib import Path
import subprocess
import tempfile
import time
import hashlib


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def verify_frozen_inputs(args, repo):
    manifest_path=repo/"runtime"/("producer-p1-target-exit-20261015-024710"if args.recovery_acceptance else "producer-shared-utility-chrome-20261015-015712")/"verification.json"
    expected_manifest_sha="c44632cd654b2a8d5312fd1a88a232856e38a593fd894a6c4a20012e0a0a55e6"if args.recovery_acceptance else "e8acc1d5d99c7ac93182c2a3b7e595b4e3fa9421a18cc18158ea0a3d3a7108a4"
    manifest_bytes = manifest_path.read_bytes()
    assert hashlib.sha256(manifest_bytes).hexdigest() == expected_manifest_sha, "Frozen producer record changed"
    prior=repo/"runtime/acceptance-graph-target-exit-recovery-20261015-023057"
    assert hashlib.sha256((prior/"evidence.json").read_bytes()).hexdigest()=="552bc44d8b36b918f35f0ccc9463eb3ff232c70798beb7c1cac5bbb340f1b8e0"
    assert hashlib.sha256((prior/"independent-failure-review.json").read_bytes()).hexdigest()=="391e07ba4604272830a7d7f97b2358985d7ffae139b405f0b86bd50ae3592dcf"
    second=repo/"runtime/acceptance-graph-target-exit-recovery-20261015-023635"
    assert hashlib.sha256((second/"evidence.json").read_bytes()).hexdigest()=="b0eb90ae9f466adc466ec481a299ed3ccaab4f9cb95e88b5910d8a0cc6eb3554"
    assert hashlib.sha256((second/"independent-failure-review.json").read_bytes()).hexdigest()=="49d0aa88bd32f158e0d72b9818ce594c0d3a4a4069015bb714d40a4bd68d9c89"
    diagnosed=repo/"runtime/acceptance-graph-target-exit-isolated-diagnostic-20261015-024258"
    assert hashlib.sha256((diagnosed/"evidence.json").read_bytes()).hexdigest()=="343810fdf58d431bf6cb18a6da769ad381b702dfb18814c65577b2ff5eedfd9e"
    assert hashlib.sha256((diagnosed/"independent-diagnostic-review.json").read_bytes()).hexdigest()=="caf0d93294c4cb70facdcb2589f493b9581b38fbfafa332b3e35f229477e97a9"
    manifest = json.loads(manifest_bytes.decode("utf-8"))
    assert manifest.get("result") == "PASS" and manifest.get("workspace_tests") == (186 if args.recovery_acceptance else 185)
    expected_sources = manifest["source_after_sha256"]
    assert len(expected_sources) == 64, "Frozen producer source manifest has an unexpected inventory"
    actual_sources = {}
    root = repo.resolve()
    for raw_path, expected in expected_sources.items():
        relative = Path(raw_path.replace("\\", "/"))
        path = (root / relative).resolve()
        path.relative_to(root)
        assert path.is_file(), f"Frozen source missing: {raw_path}"
        actual = hashlib.sha256(path.read_bytes()).hexdigest()
        assert actual == expected, f"Frozen source changed: {raw_path}"
        actual_sources[raw_path] = actual
    actual_x64 = hashlib.sha256(args.executable.resolve().read_bytes()).hexdigest()
    actual_x86 = hashlib.sha256(args.x86_executable.resolve().read_bytes()).hexdigest()
    assert actual_x64 == manifest["build_sha256"]["x64"], "x64 image is not the frozen producer build"
    assert actual_x86 == manifest["build_sha256"]["i686"], "x86 image is not the frozen producer build"
    return {"verification": str(manifest_path), "source_files": len(actual_sources),
        "source_sha256": actual_sources, "binary_sha256": {"x64": actual_x64, "i686": actual_x86}}


def workflow(args):
    lifecycle = module("target_exit_lifecycle", "test-app-lifecycle-ui.py")
    GateBase, ui, hotkeys, geometry = lifecycle.gate_type()
    actions = module("target_exit_actions", "test-connected-macro-studio-acceptance.py")
    agents = module("target_exit_agents", "test-connected-agents-acceptance.py")

    class Gate(GateBase):
        def __init__(self):
            super().__init__(args, "target-exit-isolated-diagnostic" if args.diagnostic_only else "target-exit-recovery-fixed", offline=False, count=3)
            self.clients = []
            self.gui = None
            self.descriptor = None
            self.windows = []
            self.points = []
            self.pids = []
            self.frames = {}
            self.last_frame_mtime = 0.0
            self.overlay_hwnds = set()
            self.recovery_started = False
            self.recovery_complete = False
            self.evidence.update(scope="automatic desktop; three prestarted owned targets; two virtual users and two authenticated clients")

        def save(self):
            if self.args.diagnostic_only:
                self.evidence["diagnostic_only"]=True
                if self.evidence.get("result")=="PASS":self.evidence["result"]="NOT ACCEPTED"
            super().save()

        def probes(self):
            return ui.eventually(lambda:super(Gate,self).probes(),lambda value:True,3)

        def snapshot(self):
            return ui.eventually(lambda:super(Gate,self).snapshot(),lambda value:True,3)

        def await_policy_ack(self):
            path=self.run/"ui.agent.acks.jsonl"
            def matching():
                if not path.exists():return None
                entries=[json.loads(line)for line in path.read_text(encoding="utf-8").splitlines()]
                return next((entry for entry in reversed(entries)
                    if entry["policy_enabled"] and entry["session_control"] and not entry["pending"]
                    and not entry.get("error") and set(entry["selected_users"])=={"target-exit-a","target-exit-b"}
                    and set(entry["allowed_pids"])==set(self.pids)),None)
            ack=ui.eventually(matching,bool,10)
            self.evidence["accepted_policy_ack"]=ack;self.save()

        def fixtures(self):
            geometry.physical_pixel_context()
            self.args.assigned = False
            for index in range(3):
                path = self.run / f"probe-{index}.json"
                self.owned.append(subprocess.Popen(
                    [str(self.images[0]), "probe", "--automation", "--report", str(path)],
                    creationflags=subprocess.CREATE_NO_WINDOW))
            self.pids = [process.pid for process in self.owned]

            def find_windows():
                found = {}
                @geometry.callback_type
                def visit(hwnd, unused):
                    pid = wintypes.DWORD()
                    geometry.user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
                    if pid.value in self.pids and not geometry.user32.GetWindow(hwnd, 4):
                        title = ctypes.create_unicode_buffer(96)
                        geometry.user32.GetWindowTextW(hwnd, title, len(title))
                        if title.value.startswith("ProtoInputMultiPlex Automation Test"):
                            found[pid.value] = int(hwnd)
                    return True
                assert geometry.user32.EnumWindows(visit, 0)
                return found

            found = ui.eventually(find_windows, lambda value: len(value) == 3, 30)
            geometry.user32.ClientToScreen.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.POINT)]
            for index, pid in enumerate(self.pids):
                hwnd = found[pid]
                assert geometry.user32.SetWindowPos(hwnd, wintypes.HWND(-1), 30 + 460 * index, 40, 420, 360, 0x10)
                point = wintypes.POINT(120, 100)
                assert geometry.user32.ClientToScreen(hwnd, ctypes.byref(point))
                self.windows.append(hwnd)
                self.points.append([point.x, point.y])

        def connect_two(self):
            assert self.gui and self.gui.poll() is None
            candidates = ui.eventually(lambda: list(Path(tempfile.gettempdir()).glob(
                f"protoinputmultiplex-agent-{self.gui.pid}-*.json")), lambda value: len(value) == 1, 12)
            self.descriptor = candidates[0]
            for _ in range(2):
                self.clients.append(agents.OwnedPipe(self.descriptor))
            assert self.clients[0].client != self.clients[1].client
            state = self.clients[0].request("inspect")
            assert not state.get("error")
            assert set(state["policy"]["user_ids"]) == {"target-exit-a", "target-exit-b"}
            assert set(state["policy"]["allowed_pids"]) == set(self.pids)
            assert state["policy"]["session_control"]
            self.evidence["authenticated_clients"] = [client.client for client in self.clients]
            self.evidence["fixed_client_scope"] = {"user_ids": state["policy"]["user_ids"],
                "allowed_pids": state["policy"]["allowed_pids"]}

        def inspect_users(self):
            reply = self.clients[0].request("inspect")
            assert not reply.get("error")
            states = [event["State"] for event in reply.get("events", [])
                      if isinstance(event, dict) and "State" in event]
            assert states, "Connected inspect should include the live worker state"
            return states[-1]

        def send_key(self, client, user, vk, pressed):
            scans = {116: 63, 117: 64, 118: 65}
            request = actions.key(user, pressed, vk, scans[vk])["packet"]
            reply = client.request("execute", request={"Input": request})
            assert not reply.get("error"), reply
            return reply

        def select_target(self, user, target):
            x, y = self.points[target]
            moved = self.clients[user].request("execute", request={"MoveTo": {"user": user, "x": x, "y": y}})
            assert not moved.get("error"), moved
            before = len(self.probes()[target]["mouse_events"])
            for pressed in (True, False):
                packet = actions.button(user, pressed, 0)["packet"]
                reply = self.clients[user].request("execute", request={"Input": packet})
                assert not reply.get("error"), reply
            ui.eventually(self.probes, lambda reports: len(reports[target]["mouse_events"]) >= before + 2, 10)
            reports = self.probes()
            assert [event["pressed"] for event in reports[target]["mouse_events"][-2:]] == [True, False]
            return reports

        def capture(self, name):
            # Optional one-shot metadata/copy only in either mode. Never wait,
            # resize, block native actions or claim pixel/phase freshness.
            metadata={"observed_at_unix":time.time(),"no_pixel_or_freshness_claim":True}
            try:
                hwnd=self.main_hwnd();metadata["owned_main_hwnd"]=hwnd
                if hwnd:metadata["actual_geometry"]=geometry.geometry(hwnd)
                source=self.run/"ui.png";metadata["png_mtime"]=source.stat().st_mtime
                from PIL import Image
                with Image.open(source)as image:
                    image.load();metadata["png_pixels"]=list(image.size)
                if hwnd:
                    metadata["optional_copy"]=geometry.copy_owned_product_frame(source,
                        self.run/f"optional-{name}.png",hwnd,0)
            except(OSError,ValueError,AssertionError)as frame_error:
                metadata["read_error"]=repr(frame_error)
            self.evidence.setdefault("optional_frame_observations",{})[name]=metadata
            self.save()

        def observe_failure_before_cleanup(self,error):
            # Read-only observation under one overall second. Never send a
            # routed packet, disconnect, close a window or wait on a provider.
            started=time.time();deadline=time.monotonic()+1.0
            observations=[];failure_frame=None
            while time.monotonic()<deadline:
                sample={"at_unix":time.time(),"gui_pid":self.gui.pid if self.gui else None,
                    "gui_returncode":self.gui.poll()if self.gui else None}
                for filename,label in(("ui.profile.json","profile_status"),("ui.agent.json","agent"),
                        ("ui.state.json","worker_state"),("probe-1.json","native_B")):
                    path=self.run/filename
                    try:
                        value=ui.read_json(path)
                        if label=="profile_status":value={"status":value.get("status")}
                        if label=="native_B":value={k:value.get(k)for k in("pid","held_keys","key_events","native_event_count","native_events")}
                        sample[label]={"mtime_ns":path.stat().st_mtime_ns,"value":value}
                    except(OSError,ValueError)as read_error:sample[label]={"read_error":repr(read_error)}
                observations.append(sample)
                if failure_frame is None and self.gui and self.gui.poll()is None:
                    try:
                        hwnd=self.main_hwnd()
                        if hwnd:
                            failure_frame=geometry.copy_owned_product_frame(self.run/"ui.png",
                                self.run/"failure-before-cleanup.png",hwnd,started)
                            if failure_frame:failure_frame["geometry"]=geometry.geometry(hwnd)
                    except(OSError,ValueError):pass
                remaining=deadline-time.monotonic()
                if remaining>0:time.sleep(min(.1,remaining))
            self.evidence["failure_observation_before_cleanup"]={"trigger":repr(error),
                "started_at_unix":started,"finished_at_unix":time.time(),"samples":observations,
                "own_product_failure_frame":failure_frame,
                "scope":"Read-only before client/GUI cleanup; failure frame outside four success originals."}
            self.save()

        def observe_overlays(self, sample):
            for user in sample.get("state", []):
                overlay = user.get("overlay") if isinstance(user, dict) else None
                if overlay and overlay.get("hwnd"):
                    self.overlay_hwnds.add(overlay["hwnd"])

        def exit_recovery(self):
            assert not self.recovery_started,"Never repeat a partially executed native sequence"
            self.recovery_started = True
            actor_a, actor_b = self.clients
            # The automatic-desktop fixture starts without an implicit target
            # selection; choose A and B before their first key claims.
            self.select_target(0, 0)
            self.select_target(1, 1)
            initial_state = self.inspect_users()
            assert initial_state[0]["pid"] == self.pids[0] and initial_state[1]["pid"] == self.pids[1]
            self.send_key(actor_a, 0, 117, True)  # F6 on A
            self.send_key(actor_b, 1, 118, True)  # F7 on B
            reports = ui.eventually(self.probes,
                lambda value: 117 in value[0]["held_keys"] and 118 in value[1]["held_keys"], 10)
            assert not reports[2]["key_events"]
            initial_held_reports=reports
            initial_held_state=self.inspect_users()
            assert initial_held_state[0]["pid"]==self.pids[0] and initial_held_state[1]["pid"]==self.pids[1]
            self.capture("target-exit-before")

            # Close only the owned A fixture through its real window procedure.
            geometry.user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, ctypes.c_size_t, ctypes.c_ssize_t]
            assert geometry.user32.PostMessageW(self.windows[0], 0x0010, 0, 0), "Owned WM_CLOSE to A failed"
            target_a = self.owned[0]
            target_a.wait(timeout=12)
            assert target_a.returncode == 0, "A did not exit normally after WM_CLOSE"
            if args.diagnostic_only:
                self.evidence["isolated_exit_diagnostic"]={"a_pid":self.pids[0],
                    "a_returncode":target_a.returncode,"a_closed_at_unix":time.time(),
                    "initial_held_native_reports":initial_held_reports,
                    "initial_selected_worker_state":initial_held_state}
                observation=self.evidence["isolated_exit_diagnostic"]
                try:
                    # Exactly one public-protocol Inspect; no retrying input,
                    # no continued-service/recovery/Stop acceptance criteria.
                    observation["post_exit_inspect_expected_tag"]={"client":actor_a.client,"sequence":actor_a.sequence}
                    observation["post_exit_inspect_at_unix"]=time.time()
                    observation["post_exit_inspect_reply"]=actor_a.request("inspect")
                except Exception as inspect_error:
                    observation["post_exit_inspect_error"]=repr(inspect_error)
                self.save()
                self.observe_failure_before_cleanup("isolated target-exit diagnosis; no acceptance claim")
                self.recovery_complete=True;return
            assert self.gui.poll() is None, "The GUI/worker must remain alive after automatic target exit"

            states = ui.eventually(self.inspect_users,
                lambda value: len(value) == 2 and value[0]["pid"] == 0 and value[0]["keyboard_pid"] == 0
                and not value[0]["captured"] and "exited" in value[0]["status"].lower()
                and value[1]["pid"] == self.pids[1] and 118 in value[1]["keys"], 12)
            after_exit_state=states
            assert states[0]["status"]
            self.capture("target-exit-a-cleared")

            # B remains serviceable under a fresh tap while its independent F7
            # claim stays held. A's raw F6 is not expected to reach dead A.
            reports = self.probes()
            b_before = len(reports[1]["key_events"])
            self.send_key(actor_b, 1, 116, True)  # F5 tap on B
            self.send_key(actor_b, 1, 116, False)
            reports = ui.eventually(self.probes, lambda value:
                118 in value[1]["held_keys"] and len(value[1]["key_events"]) >= b_before + 2, 10)
            assert [event["pressed"] for event in reports[1]["key_events"][-2:]] == [True, False]
            assert len(reports[1]["key_events"])==b_before+2
            assert all(event["vk"]==116 for event in reports[1]["key_events"][-2:])
            b_continuity_reports=reports
            self.capture("target-exit-b-continues")

            # A's correlated ReleaseAll is client-scoped; B's F7 must survive.
            release = actor_a.request("execute", request="ReleaseAll")
            assert release["acknowledged"] and release["client"] == actor_a.client and not release.get("error")
            states = ui.eventually(self.inspect_users, lambda value: 117 not in value[0]["keys"]
                and 118 in value[1]["keys"], 10)
            after_release_state=states
            reports = ui.eventually(self.probes, lambda value: 118 in value[1]["held_keys"], 10)
            assert reports[2]["pid"] == self.pids[2] and 118 in reports[1]["held_keys"]

            # C was preauthorized from startup but never implicitly selected.
            # An explicit owned click selects C, followed by a fresh F6 pair.
            self.select_target(0, 2)
            selected = ui.eventually(self.inspect_users,
                lambda value: value[0]["pid"] == self.pids[2] and value[0]["keyboard_pid"] == self.pids[2], 10)
            c_before = len(self.probes()[2]["key_events"])
            self.send_key(actor_a, 0, 117, True)
            reports = ui.eventually(self.probes, lambda value: 117 in value[2]["held_keys"], 10)
            self.send_key(actor_a, 0, 117, False)
            reports = ui.eventually(self.probes, lambda value: 117 not in value[2]["held_keys"]
                and len(value[2]["key_events"]) >= c_before + 2, 10)
            assert len(reports[2]["key_events"])==c_before+2
            assert all(event["vk"]==117 for event in reports[2]["key_events"][-2:])
            assert [event["pressed"] for event in reports[2]["key_events"][-2:]] == [True, False]
            assert 118 in reports[1]["held_keys"]
            self.evidence["target_exit_and_recovery"] = {
                "a_returncode": target_a.returncode,
                "initial_held_native_reports": initial_held_reports,
                "initial_held_worker_state": initial_held_state,
                "initial_selected_targets": initial_state,
                "worker_after_exit": after_exit_state,
                "b_tap_and_held_f7": b_continuity_reports[1],
                "client_a_release_all": release,
                "after_release_all": after_release_state,
                "logical_claim_evidence_limit":"Worker keys reflect native holds, not raw claim telemetry; fresh C F6 pair is the functional cleanup proof.",
                "c_selected": selected[0],
                "c_f6_pair": reports[2]["key_events"][-2:],
            }
            self.recovery_complete=True
            self.save()

    # Gate construction only copies the pinned executables and allocates an
    # evidence directory. Verify the frozen record before any process starts.
    producer = verify_frozen_inputs(args, Path(__file__).resolve().parent.parent)
    gate = Gate()
    gate.evidence["frozen_producer"] = producer
    gate.evidence["runner_sha256"] = hashlib.sha256(Path(__file__).resolve().read_bytes()).hexdigest()
    ids = ["target-exit-a", "target-exit-b"]
    step = ui.step
    profile = {"version": 1, "concurrent": True,
        "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": []},
        "replication": {"version": 1, "edges": []},
        "seats": [{"id": ids[index], "name": f"Exit recovery {index + 1}",
            "mouse": f"virtual://mouse/{index + 1}", "keyboard": f"virtual://keyboard/{index + 1}",
            "desktop_start": None, "cursor": {"lock_window": False}}
            for index in range(2)]}
    steps = [step("start", "Start routing"), step("settings", "Settings"),
        step("agent-category", "Settings category Agent access"), step("agent-page", "Agent Access"),
        step("enable-access", "Enable agent access", {"agent_users": ",".join(ids),
            "agent_pids": ",".join(map(str, gate.pids)), "agent_session_control": "false",
            "agent_capture": "false", "agent_app_preferences": "false"}),
        step("session-control", "Agent session control permission"),
        step("apply-policy", "Apply access policy"), step("home", "Back to Home"),
        step("stop", "Stop routing")]
    if args.diagnostic_only:
        steps=steps[:8]
        gate.evidence["scope"]="DIAGNOSTIC ONLY: eight setup controls, A/B native holds, close A, one Inspect and <=1second read-only precleanup observation. No B tap, C recovery, normal Stop or P1 acceptance."
    assert len(steps)==(8 if args.diagnostic_only else 9)

    def verify(name, sample):
        gate.observe_overlays(sample)
        if name == "start":
            gate.gui = gate.owned[-1]
            assert gate.gui.poll() is None
            assert len(gate.pids) == 3 and len(set(gate.pids)) == 3
            if len(sample["state"])!=2:return False
            handles=[u["overlay"]["hwnd"]for u in sample["state"]]
            if not all(handles)or len(set(handles))!=2 or not all(geometry.user32.IsWindow(h)for h in handles):return False
            gate.overlay_hwnds.update(handles)
            gate.evidence["initial_live_overlay_hwnds"]=handles
        elif name == "apply-policy":
            gate.await_policy_ack()
            try:gate.connect_two()
            except(OSError,ValueError)as error:
                raise AssertionError(f"Connection IO failure; do not repeat partial registration: {error!r}")from error
        elif name == "home":
            try:gate.exit_recovery()
            except(OSError,ValueError)as error:
                raise AssertionError(f"Native sequence IO failure; do not replay partial actions: {error!r}")from error
        elif name == "stop":
            reports = sample["probes"]
            # The normal GUI Stop sends B's ordinary F7-up; never synthesize it
            # from cleanup code. Its event must appear on still-live B.
            reports = ui.eventually(gate.probes, lambda value:
                118 not in value[1]["held_keys"]
                and any(e["vk"] == 118 and not e["pressed"] for e in value[1]["key_events"]), 10)
            ups = [event for event in reports[1]["key_events"] if event["vk"] == 118 and not event["pressed"]]
            assert len(ups) == 1
            assert ups[0]["scan"] == 65 and not ups[0]["extended"] and not ups[0]["alt"], \
                f"Stop must deliver a plain F7 key-up: {ups[0]}"
            ui.eventually(lambda:not list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{gate.gui.pid}-*.json"))
                and all(not geometry.user32.IsWindow(hwnd)for hwnd in gate.overlay_hwnds),bool,10)
            assert len(gate.overlay_hwnds)==2
            for client in gate.clients:
                client.close()
            assert all(client.handle is None for client in gate.clients)
            gate.clients.clear()
            gate.capture("routing-stopped")
            gate.evidence["stop"] = {"plain_f7_up_count": len(ups),
                "overlay_windows_closed": True, "agent_descriptor_removed": True,
                "connected_handles_closed": True}
        return True

    def before_controls(current):
        return current.main_hwnd() is not None

    def after_controls(current):
        if args.diagnostic_only:
            assert current.recovery_complete
            # Cleanup remains in finally. Any B UP from those disconnects or
            # closes is unattributed and is never normal-Stop evidence.
            return True
        # Only close owned fixtures after every UI event/sample has been
        # accepted. The stop observation above checks the ordinary F7-up.
        for process, window in zip(current.owned[:3], current.windows):
            if process.poll() is None:
                assert geometry.user32.PostMessageW(window, 0x0010, 0, 0), "Owned fixture WM_CLOSE failed"
                process.wait(timeout=12)
                assert process.returncode == 0
        hwnd = current.main_hwnd()
        assert hwnd, "Owned GUI main window disappeared before normal close"
        current.expected_exit = True
        assert geometry.user32.PostMessageW(hwnd, 0x0010, 0, 0), "Owned GUI WM_CLOSE failed"
        current.gui.wait(timeout=12)
        assert current.gui.returncode == 0
        assert not hotkeys.lifecycle_window(current.gui.pid)
        assert all(process.poll() is not None for process in current.owned)
        assert current.recovery_complete
        current.evidence["owned_processes_closed"] = True
        return True

    try:
        provenance=gate.run/"runner-source";provenance.mkdir()
        for name in(Path(__file__).name,"test-connected-agents-acceptance.py","test-connected-macro-studio-acceptance.py","owned-gui-geometry.py"):
            shutil.copy2(Path(__file__).with_name(name),provenance/name)
        gate.save()
        gate.fixtures()
        profile["desktop"]["allowed_pids"] = list(gate.pids)
        steps[4]["fields"]["agent_pids"] = ",".join(map(str, gate.pids))
        gate.evidence["preauthorized_pids"] = list(gate.pids)
        preferences = lifecycle.preference_defaults()
        preferences.update(launch_minimized=False, minimize_to_tray=False, close_to_tray=False,
            launch_at_login=False, shortcuts=[])
        gate.execute(profile, preferences, steps, verify,
            before_controls=before_controls, after_controls=after_controls)
        gate.evidence["result"] = "NOT ACCEPTED" if args.diagnostic_only else "PENDING INDEPENDENT DATA REVIEW"
        if args.diagnostic_only:gate.evidence["diagnostic_only_complete"]=True
        else:gate.evidence["control_native_result"]="PASS"
        gate.evidence["ui_observations"] = len(steps)
        gate.evidence["frames"] = gate.frames
        gate.save()
        return gate.run
    except Exception as error:
        try:gate.observe_failure_before_cleanup(error)
        except Exception as observation_error:
            gate.evidence["failure_observation_error"]=repr(observation_error)
        gate.evidence["failure"] = repr(error)
        gate.evidence["result"] = "NOT ACCEPTED"
        gate.save()
        raise
    finally:
        for client in gate.clients:
            client.close()
        gate.evidence["connected_clients_closed"] = all(client.handle is None for client in gate.clients)
        try:
            gate.close()
        finally:
            lifecycle.remove_startup_key(gate.startup_key)
            gate.evidence["owned_startup_key_removed"] = lifecycle.startup_value(gate.startup_key) is None
            gate.evidence["owned_processes_closed"] = all(process.poll() is not None for process in gate.owned)
            gate.save()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready", action="store_true")
    mode=parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--diagnostic-only",action="store_true")
    mode.add_argument("--recovery-acceptance",action="store_true")
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    args = parser.parse_args()
    assert args.ready, "Await exact mode/source/image ROOT READY and exclusive native slot"
    workflow(args)


if __name__ == "__main__":
    main()
