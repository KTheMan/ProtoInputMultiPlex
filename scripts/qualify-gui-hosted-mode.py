"""One disposable GUI-owned hosted routing/viewport gate on GitHub-hosted Windows.

Private event descriptors, live references and DevTools payloads remain in the
runner-owned directory. The public receipt contains bounded decisions only.
"""
import argparse
import ctypes
from ctypes import wintypes
import http.server
import importlib.util
import json
import os
from pathlib import Path
import queue
import re
import shutil
import subprocess
import threading
import time
from urllib.parse import urlsplit


spec = importlib.util.spec_from_file_location("pim_hosted_window_base", Path(__file__).with_name("qualify-gui-owned-window.py"))
owned = importlib.util.module_from_spec(spec)
spec.loader.exec_module(owned)
base = owned.base
require = base.require


class HostedModePair(owned.WindowPair):
    def __init__(self, args):
        super().__init__(args)
        self.end = self.start + 240
        self.result = {
            "schema": "pim.gui-hosted-mode.hosted.v1", "source_commit": args.commit,
            "status": "FAIL", "scope": "one virtual-only GUI router and owned host viewport",
            "full_sdk_complete": False, "release_qualified": False,
            "hardware_qualified": False, "web_mode_worker_receipts": 0,
            "web_viewport_receipts": 0, "mcp_mode_worker_receipts": 0,
            "native_viewport_receipts": 0, "native_completed": False,
            "native_retired": False, "native_retirement_phase": "not_started",
            "browser_acceptance_phase": "not_started", "browser_helper_failure": "none",
            "old_revision_rejected": False, "old_epoch_rejected": False,
            "event_epoch_retained": False,
            "routing_generation_advanced": False, "ordinary_quit_clean": False,
            "pending_refresh_correlated": False, "profile_file_unchanged": False,
            "preferences_file_unchanged": False,
            "lost_receipt_deadline": False, "fixture_injected_gap": False,
            "browser_no_replay": False, "stale_timer_reconnect_isolated": False,
        }

    def launch_status(self):
        # The earlier GUI-pair gate expects a stopped controller. This gate
        # explicitly observes both live routing and the later clean Native Stop.
        progress, progress_modified = self.live_json("gui_progress", self.ui / "prepare.progress.json")
        require(time.time() - progress_modified < 2 and progress["io"]["fault"] is None)
        value, modified = self.live_json("gui_launch", self.ui / "prepare.launch.json")
        require(time.time() - modified < 2 and "controller_present" in value and "ready" in value)
        return value

    def issue_host_descriptor(self, slot, live_count):
        paths = self.descriptors()
        if len(paths) != live_count:
            return None
        new = paths - self.seen_descriptors
        require(len(new) == 1)
        path = new.pop()
        value = self.strict_json("authority_descriptor", path)
        require(set(value) == {"protoinputmultiplex_application_events", "endpoint",
                               "exact_origin", "credential", "expires_unix_ms"})
        require(value["protoinputmultiplex_application_events"] == 1 and
                value["exact_origin"] == "http://127.0.0.1:4173")
        endpoint = urlsplit(value["endpoint"])
        require(endpoint.scheme == "ws" and endpoint.hostname == "127.0.0.1" and
                endpoint.port and endpoint.path == "/application-events/v1" and
                not (endpoint.query or endpoint.fragment or endpoint.username or endpoint.password))
        require(re.fullmatch("[0-9a-fA-F]{64}", value["credential"]) is not None)
        require(250000 < value["expires_unix_ms"] - time.time() * 1000 <= 301000)
        match = re.fullmatch(rf"protoinputmultiplex\.application-events\.{self.gui.pid}\.(\d+)\.(\d+)\.json", path.name)
        require(match is not None)
        epoch = int(match[1])
        if slot == 1:
            self.epoch, self.endpoint, self.credentials = epoch, value["endpoint"], set()
        require(epoch == self.epoch and value["endpoint"] == self.endpoint and
                value["credential"] not in self.credentials)
        self.credentials.add(value["credential"])
        self.seen_descriptors.add(path)
        self.connection_paths[slot] = path
        return True

    def sample(self, step):
        if step == 6:
            status = self.event_status("local", 0, 0)
            return status and not status["pointer_export"] and not status["keyboard_export"]
        if step in (10, 23, 28, 32, 35):
            status = self.event_status("local" if step == 10 else "active", 0,
                                       {10: 1, 23: 1, 28: 2, 32: 1, 35: 2}[step])
            return status and not status["pointer_export"] and not status["keyboard_export"]
        if step == 18:
            return self.event_status("active", 0, 1, 0)
        if step == 19:
            if not self.issue_host_descriptor(1, 1):
                return None
            self.stage = "native_host_viewport"
            self.native_report = self.private / "native-hosted.jsonl"
            self.native = self.launch([self.native_exe, "--dll", self.dll, "--sha256",
                self.native_meta["dll"]["sha256"], "--descriptor", self.connection_paths[1],
                "--report", self.native_report, "--hosted-route", "--observe-seconds", "20"],
                "native-hosted")
            self.result["native_retirement_phase"] = "viewport_receipts"
            self.wait(self.native_detail_done, 20)
            self.result["native_retirement_phase"] = "native_exit_wait"
            native_exit = self.native.wait(timeout=35)
            self.result["native_retirement_phase"] = "native_exit_zero"
            require(native_exit == 0)
            self.result["native_retirement_phase"] = "native_unload_markers"
            require(self.native_detail("dll_unloaded_before_owner_exit") and
                    self.native_detail("owner_ready_to_exit_after_unload"))
            self.result["native_retirement_phase"] = "host_retired_no_connections"
            # A disconnected receiver loses its approval; later counts include only live rows.
            self.wait(lambda: self.event_status("active", 0, 0, 0))
            self.result["native_retirement_phase"] = "retired"
            self.result["native_retired"] = True
            return True
        if step == 24:
            return self.issue_host_descriptor(2, 1)
        if step == 29:
            if not self.issue_host_descriptor(3, 2):
                return None
            self.stage = "active_mcp_observer"
            self.retired_mcp_descriptor = self.private / "retired-mcp.json"
            shutil.copyfile(self.connection_paths[3], self.retired_mcp_descriptor)
            self.mcp_slot = 3
            self.rpc_start()
            reply, value = self.hosted_tool("inspect")
            require(not reply.get("isError") and value["hosted"]["phase"] == "native")
            self.wait(lambda: self.event_status("active", 0, 2, 1))
            return True
        if step == 30:
            self.stage = "browser_mode_and_viewport"
            self.browser_start()
            self.browser_acceptance()
            require(not self.connection_paths[3].exists())
            self.mcp.stdin.close()
            require(self.mcp.wait(timeout=8) == 0)
            stale = self.launch([self.exe, "mcp", "--event-connection", self.retired_mcp_descriptor],
                                "retired-mcp")
            require(stale.wait(timeout=8) != 0)
            self.web("window.pimClient.disconnect(); true")
            self.wait(lambda: self.event_status("active", 0, 0, 0))
            require(not self.descriptors())
            return True
        if step == 33:
            return self.issue_host_descriptor(4, 1)
        if step == 36:
            return self.issue_host_descriptor(5, 2)
        raise base.Rejected()

    def native_detail_done(self):
        actions = [row["detail"] for row in self.native_rows() if row["stage"] == "native_hosted_action"]
        completed = self.native_detail("native_hosted_completed")
        ready = self.native_detail("gui_ready")
        if len(actions) != 2 or not completed or not ready:
            return None
        events, controls = ready["events"], ready["controls"]
        empty = {"kind": "only", "users": []}
        require(ready["version"] == 1 and ready["epoch"] == self.epoch and
                events["users"] == empty and controls["users"] == empty and
                events.get("host_state") is True and controls.get("host_viewport_control") is True and
                not controls.get("host_mode_control", False) and
                not events["pointer_events"] and not events["key_events"] and
                not events["owned_windows"])
        require(all(not controls.get(key, False) for key in (
            "delegated_users", "user_lifecycle", "cursor", "pointer_input", "key_input",
            "window_publication", "window_identity_metadata", "window_tracking", "window_control")))
        require(actions[0]["action"] == "minimize" and actions[0]["observed_phase"] == "native" and
                actions[0]["observed_minimized"] is True and actions[0]["later_observed"] and
                actions[0]["native_input_enabled"] is False)
        require(actions[1]["action"] == "restore" and actions[1]["observed_phase"] == "native" and
                actions[1]["observed_minimized"] is False and actions[1]["later_observed"] and
                actions[1]["native_input_enabled"] is False)
        require(type(actions[0]["observed_revision"]) is int and
                type(actions[1]["observed_revision"]) is int and
                0 < actions[0]["observed_revision"] < actions[1]["observed_revision"])
        require(completed == {"actions": 2, "event_epoch": self.epoch,
                              "final_revision": actions[1]["observed_revision"],
                              "no_virtual_users": True})
        self.result["native_viewport_receipts"] = 2
        self.result["native_completed"] = True
        return True

    def native_retirement_diagnostic(self):
        # Only fixed enums, booleans and a bounded OS exit code leave the runner.
        diagnostic = {"schema": "pim.gui-hosted-mode.native-retirement.v1",
                      "native_exit_code": None, "last_stage": "none",
                      "last_progress_stage": "none", "failure_reason": "none",
                      "failed_record_observed": False, "dll_unload_observed": False,
                      "owner_ready_observed": False, "diagnostic_read_failed": False}
        self.result["native_retirement_diagnostic"] = diagnostic
        stages = {
            "started", "dll_hash_verified", "dll_exports_resolved", "owner_hidden_window_created",
            "client_connected_completion_retained", "gui_ready", "command_locally_admitted",
            "command_remotely_completed", "command_remotely_rejected", "native_hosted_action",
            "native_hosted_completed", "stream_gap", "stream_ended", "hwnd_destroyed_stop_returned",
            "initial_destroy_schedule", "actual_worker_os_thread_signaled_after_hwnd_death",
            "client_destroyed_completion_closed", "dll_unloaded_before_owner_exit",
            "owner_ready_to_exit_after_unload", "failed",
        }
        reasons = {
            "native hosted receiver ended during overlap": "hosted_overlap_ended",
            "DestroyWindow failed": "destroy_window_failed",
            "Stop was not successful in WM_NCDESTROY": "stop_not_successful",
            "Stop must report local termination with remote cleanup unconfirmed": "stop_terminal_state",
            "Stop must clear local model": "stop_model_not_empty",
            "Destroy returned neither BUSY nor OK": "destroy_status",
            "CloseHandle failed": "completion_close_failed", "DPI restore failed": "dpi_restore_failed",
            "FreeLibrary failed": "free_library_failed",
            "DLL still loaded after FreeLibrary": "dll_still_loaded",
            "wake contains payload": "wake_payload",
            "fixture incoming message budget exceeded": "message_budget",
            "invalid server message": "server_message_invalid",
            "invalid retained length": "retained_length_invalid",
            "response length mismatch": "response_length_mismatch",
            "invalid retained JSON": "retained_json_invalid",
            "fixture I/O or encoding failure": "fixture_io_encoding",
            "report lock poisoned": "report_lock_poisoned",
            "bounded owner wait timed out": "owner_wait_deadline",
            "message/completion wait failed": "owner_wait_failed",
            "unexpected owner quit before cleanup": "owner_quit_before_cleanup",
            "owner panic; no unload safety established": "owner_panic",
        }
        status_actions = {
            "query for discard", "response discard", "discard consumes response",
            "retain before window death", "Stop retained-output fence", "terminal query after Stop",
            "final destroy", "destroy consumes token", "second destroy invalid", "drain",
            "response_size", "response_read", "full read consumes output",
        }
        try:
            code = self.native.poll()
            diagnostic["native_exit_code"] = code if type(code) is int and -2147483648 <= code <= 4294967295 else None
            for row in self.native_rows():
                if not isinstance(row, dict):
                    diagnostic["diagnostic_read_failed"] = True
                    continue
                stage = row.get("stage")
                safe_stage = stage if isinstance(stage, str) and stage in stages else "unrecognized"
                diagnostic["last_stage"] = safe_stage
                if stage == "dll_unloaded_before_owner_exit":
                    diagnostic["dll_unload_observed"] = True
                if stage == "owner_ready_to_exit_after_unload":
                    diagnostic["owner_ready_observed"] = True
                if stage != "failed":
                    diagnostic["last_progress_stage"] = safe_stage
                    continue
                diagnostic["failed_record_observed"] = True
                detail = row.get("detail")
                reason = detail.get("reason") if isinstance(detail, dict) else None
                diagnostic["failure_reason"] = "unrecognized"
                if isinstance(reason, str):
                    diagnostic["failure_reason"] = reasons.get(reason, "unrecognized")
                    if any(re.fullmatch(re.escape(action) + r": expected -?[0-9]{1,10}, got -?[0-9]{1,10}", reason)
                           for action in status_actions):
                        diagnostic["failure_reason"] = "abi_cleanup_or_drain_status"
        except BaseException:
            # Never replace the original acceptance failure with a diagnostic error.
            diagnostic["diagnostic_read_failed"] = True

    def browser_start(self):
        self.result["browser_acceptance_phase"] = "edge_executable"
        edge = Path(os.environ["ProgramFiles(x86)"]) / "Microsoft/Edge/Application/msedge.exe"
        require(edge.is_file())
        profile = self.private / "edge-profile"
        self.result["browser_acceptance_phase"] = "browser_launch"
        self.browser = self.launch([edge, "--headless=new", "--window-size=1280,1000",
            "--disable-gpu", "--disable-extensions", "--no-first-run",
            "--no-default-browser-check", "--remote-debugging-address=127.0.0.1",
            "--remote-debugging-port=0", "--user-data-dir=" + str(profile), "about:blank"], "edge")
        def devtools():
            raw, _ = self.live_bytes("browser_devtools", profile / "DevToolsActivePort", 4096)
            lines = raw.decode("utf-8-sig").splitlines()
            return lines if len(lines) == 2 and lines[0].isdigit() else None
        self.result["browser_acceptance_phase"] = "devtools_wait"
        lines = self.wait(devtools)
        self.result["browser_acceptance_phase"] = "cdp_connect"
        self.cdp = base.CDP("ws://127.0.0.1:" + lines[0] + lines[1])
        self.result["browser_acceptance_phase"] = "cdp_target_create"
        target = self.cdp.call("Target.createTarget", {"url": "http://127.0.0.1:4173/hosted.html"})["targetId"]
        self.result["browser_acceptance_phase"] = "cdp_target_attach"
        self.session = self.cdp.call("Target.attachToTarget", {"targetId": target, "flatten": True})["sessionId"]
        self.result["browser_acceptance_phase"] = "helper_ready_wait"
        self.wait(lambda: self.web("!!window.pimClient && !!window.runHostedModeAcceptance"))
        self.result["browser_acceptance_phase"] = "descriptor_read"
        descriptor = self.strict_json("authority_descriptor", self.connection_paths[2])
        self.result["browser_acceptance_phase"] = "browser_connect"
        self.web("window.pimClient.connect(" + json.dumps(descriptor["endpoint"]) + "," + json.dumps(descriptor["credential"]) + "); true")
        del descriptor
        self.result["browser_acceptance_phase"] = "initial_state_wait"
        state = self.wait(self.state)
        self.result["browser_acceptance_phase"] = "initial_native_state"
        require(state["epoch"] == self.epoch and state["users"] == [] and state["hosted"]["phase"] == "native")
        self.result["browser_acceptance_phase"] = "host_only_grants"
        require(state["event_grant"]["host_state"] and state["control_grant"]["host_mode_control"] and state["control_grant"]["host_viewport_control"])
        self.result["browser_acceptance_phase"] = "event_input_disabled"
        require(not state["event_grant"]["pointer_events"] and not state["event_grant"]["key_events"])
        self.result["browser_acceptance_phase"] = "control_input_disabled"
        require(not state["control_grant"]["pointer_input"] and not state["control_grant"]["key_input"])
        self.native_generation = state["hosted"]["routing_generation"]
        self.result["browser_acceptance_phase"] = "initial_two_client_status"
        self.wait(lambda: self.event_status("active", 0, 2, 2))

    def browser_acceptance(self):
        # Exact existing helper errors become fixed enums; never export error text.
        helper_failures = {
            "FAIL: host-only receiver authority": "host_only_authority",
            "FAIL: initial native observation": "initial_native_observation",
            "FAIL: event frame budget": "event_frame_budget",
            "FAIL: wrong or uncertain host receipt": "wrong_or_uncertain_receipt",
            "FAIL: hosted receipt deadline": "hosted_receipt_deadline",
            "FAIL: live host observation missing": "live_observation_missing",
            "FAIL: stale host reference": "stale_host_reference",
            "FAIL: exact hosted receipt revision missing": "receipt_revision_missing",
            "FAIL: hosted query was not native": "query_not_native",
            "FAIL: switched worker completion": "switched_worker_completion",
            "FAIL: stale hosted revision was accepted": "stale_revision_accepted",
            "FAIL: concurrent worker completion": "concurrent_worker_completion",
            "FAIL: minimize observation": "minimize_observation",
            "FAIL: ordered hosted stream did not reconcile": "stream_not_reconciled",
            "FAIL: restore observation": "restore_observation",
        }
        self.result["browser_acceptance_phase"] = "helper_start"
        self.web("window.pimGate=null; window.runHostedModeAcceptance(window.pimClient).then(x=>window.pimGate=x,e=>{const known=" + json.dumps(helper_failures) + "; window.pimGate={status:'FAIL',failure:typeof e?.message==='string'&&Object.prototype.hasOwnProperty.call(known,e.message)?known[e.message]:'unrecognized'};}); true")
        self.result["browser_acceptance_phase"] = "helper_result_wait"
        result = self.wait(lambda: self.web("window.pimGate"), 85)
        self.result["browser_acceptance_phase"] = "helper_pass_predicate"
        if isinstance(result, dict) and result.get("status") == "FAIL":
            failure = result.get("failure")
            self.result["browser_helper_failure"] = failure if failure in helper_failures.values() else "unrecognized"
        require(result == {"status": "PASS", "query": True, "switched": True,
                          "concurrent": True, "minimize": True, "restore": True,
                          "stale_revision": True, "refresh_after_receipt": True})
        self.result["browser_acceptance_phase"] = "final_state_wait"
        state = self.wait(self.state)
        self.result["browser_acceptance_phase"] = "final_concurrent_state"
        require(state["epoch"] == self.epoch and state["hosted"]["phase"] == "ready" and state["hosted"]["mode"] == "concurrent")
        self.result["browser_acceptance_phase"] = "routing_generation_advanced"
        require(state["hosted"]["routing_generation"] > self.native_generation)
        self.result["browser_acceptance_phase"] = "launch_status_read"
        launch = self.launch_status()
        self.result["browser_acceptance_phase"] = "controller_ready"
        require(launch["controller_present"] and launch["ready"] and not launch["stopping"])
        self.result["browser_acceptance_phase"] = "requester_only_status"
        self.wait(lambda: self.event_status("active", 0, 1, 1))
        self.result["browser_acceptance_phase"] = "requester_only_descriptor"
        require(self.descriptors() == {self.connection_paths[2]})
        self.web_generation = state["hosted"]["routing_generation"]
        self.result["web_mode_worker_receipts"] = 2
        self.result["web_viewport_receipts"] = 2
        self.result["old_revision_rejected"] = True

    def rpc_start(self):
        err = open(self.private / "mcp-hosted-stderr.log", "xb")
        self.mcp = subprocess.Popen([str(self.exe), "mcp", "--event-connection", str(self.connection_paths[self.mcp_slot])],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err, text=True,
            encoding="utf-8", creationflags=subprocess.CREATE_NO_WINDOW)
        err.close()
        self.mcp_queue = queue.Queue()
        def reader():
            try:
                for line in self.mcp.stdout:
                    require(len(line) <= 4 * 1024 * 1024)
                    self.mcp_queue.put(json.loads(line))
            finally:
                self.mcp_queue.put(None)
        threading.Thread(target=reader, daemon=True).start()
        self.rpc_id = 0
        self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                 "clientInfo": {"name": "pim-hosted-gate", "version": "1"}})
        self.mcp.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
        self.mcp.stdin.flush()

    def rpc(self, method, params):
        self.rpc_id += 1
        self.mcp.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.rpc_id,
                                         "method": method, "params": params}) + "\n")
        self.mcp.stdin.flush()
        hosted_action = (method == "tools/call" and params.get("name") == "hosted_session" and
            params.get("arguments", {}).get("operation") in {"set_mode", "minimize", "restore"})
        until = min(self.end, time.monotonic() + (70 if hosted_action else 8))
        while time.monotonic() < until:
            self.deadline()
            try:
                result = self.mcp_queue.get(timeout=.1)
            except queue.Empty:
                continue
            require(result is not None and "error" not in result)
            if result.get("id") == self.rpc_id:
                return result["result"]
        raise base.Rejected()

    def hosted_tool(self, operation, revision=None, mode=None, epoch=None):
        args = {"operation": operation, "epoch": self.epoch if epoch is None else epoch}
        if revision is not None:
            args["revision"] = revision
        if mode is not None:
            args["mode"] = mode
        reply = self.rpc("tools/call", {"name": "hosted_session", "arguments": args})
        return reply, reply.get("structuredContent")

    def mcp_acceptance(self):
        self.mcp_slot = 5
        self.rpc_start()
        self.wait(lambda: self.event_status("active", 0, 2, 1))
        self.browser_reconnect()
        stale_epoch = self.epoch - 1 if self.epoch > 1 else 2
        stale, _ = self.hosted_tool("inspect", epoch=stale_epoch)
        require(stale.get("isError") is True)
        self.result["old_epoch_rejected"] = True
        reply, inspected = self.hosted_tool("inspect")
        require(not reply.get("isError") and inspected["epoch"] == self.epoch)
        hosted = inspected["hosted"]
        require(hosted["phase"] == "ready" and hosted["mode"] == "concurrent" and
                hosted["routing_generation"] == self.web_generation)
        self.browser_lost_receipt()
        reply, inspected = self.hosted_tool("inspect")
        require(not reply.get("isError") and inspected["hosted"]["mode"] == "switched" and
                inspected["hosted"]["routing_generation"] == self.web_generation)
        hosted = inspected["hosted"]
        reply, native = self.hosted_tool("set_mode", hosted["revision"], "native")
        require(not reply.get("isError") and native["action"] == {"kind": "set_mode", "value": "native"})
        require(native["hosted"]["phase"] == "native" and native["hosted"]["mode"] == "native")
        require(native["hosted"]["routing_generation"] == self.web_generation)
        self.wait(lambda: (value if value and not value["controller_present"] and not value["stopping"] else None)
                  if (value := self.launch_status()) else None)
        require(self.event_status("active", 0, 1, 1))
        require(self.descriptors() == {self.connection_paths[5]})
        self.result["event_epoch_retained"] = True
        refresh = self.rpc("tools/call", {"name": "native_windows", "arguments": {"operation": "refresh"}})
        require(not refresh.get("isError"))
        reply, observed = self.hosted_tool("inspect")
        require(not reply.get("isError") and observed["hosted"]["phase"] == "native")
        reply, concurrent = self.hosted_tool("set_mode", observed["hosted"]["revision"], "concurrent")
        require(not reply.get("isError") and concurrent["action"] == {"kind": "set_mode", "value": "concurrent"})
        require(concurrent["hosted"]["phase"] == "ready" and concurrent["hosted"]["mode"] == "concurrent")
        require(concurrent["hosted"]["routing_generation"] > self.web_generation)
        self.wait(lambda: (value if value and value["controller_present"] and value["ready"] and not value["stopping"] else None)
                  if (value := self.launch_status()) else None)
        require(self.event_status("active", 0, 1, 1) and self.descriptors() == {self.connection_paths[5]})
        self.result["mcp_mode_worker_receipts"] = 2
        self.result["routing_generation_advanced"] = True

    def browser_lost_receipt(self):
        self.stage = "browser_lost_receipt"
        self.web("window.pimFault=null; window.runHostedLostReceipt(window.pimClient).then(x=>window.pimFault=x,()=>window.pimFault={status:'FAIL'}); true")
        self.wait(lambda: self.web("window.pimHostFilter?.dropped && window.pimHostFilter?.refreshed"), 20)
        reply, value = self.hosted_tool("inspect")
        require(not reply.get("isError") and value["hosted"]["phase"] == "ready" and
                value["hosted"]["mode"] == "switched" and
                value["hosted"]["routing_generation"] == self.web_generation)
        launch = self.launch_status()
        require(launch["controller_present"] and launch["ready"] and not launch["stopping"])
        require(self.event_status("active", 0, 2, 2))
        result = self.wait(lambda: self.web("window.pimFault"), 70)
        require(result == {"status": "PASS", "dropped_reply": True,
                           "fixture_injected_gap": True, "explicit_refresh": True,
                           "browser_deadline": True, "no_replay": True,
                           "stale_timer_reconnect": True})
        reply, after = self.hosted_tool("inspect")
        require(not reply.get("isError") and after["hosted"]["mode"] == "switched" and
                after["hosted"]["routing_generation"] == value["hosted"]["routing_generation"] and
                after["hosted"]["revision"] == value["hosted"]["revision"])
        self.wait(lambda: self.event_status("active", 0, 1, 1))
        require(self.descriptors() == {self.connection_paths[5]})
        self.result["lost_receipt_deadline"] = True
        self.result["fixture_injected_gap"] = True
        self.result["browser_no_replay"] = True
        self.result["pending_refresh_correlated"] = True
        self.result["stale_timer_reconnect_isolated"] = True
        self.stage = "mcp_native_stop_restart"

    def browser_reconnect(self):
        require(self.connection_paths[4] != self.connection_paths[2])
        descriptor = self.strict_json("authority_descriptor", self.connection_paths[4])
        self.web("window.pimClient.connect(" + json.dumps(descriptor["endpoint"]) + "," +
                 json.dumps(descriptor["credential"]) + "); true")
        del descriptor
        state = self.wait(self.state)
        require(state["epoch"] == self.epoch and state["hosted"]["phase"] == "ready" and
                state["hosted"]["mode"] == "concurrent")
        self.wait(lambda: self.event_status("active", 0, 2, 2))
        self.wait(lambda: self.web("Date.now()-window.pimMatchedTimerStarted>=8000"), 15)
        require(self.web("Date.now()-window.pimMatchedTimerStarted<30000"))

    def close_gui(self):
        self.close_gui_owned_main()
        require(self.gui.wait(timeout=20) == 0 and not self.descriptors())

    def run(self):
        args = self.args
        require(os.name == "nt" and os.environ.get("GITHUB_ACTIONS") == "true" and
                os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted")
        require(re.fullmatch("[0-9a-f]{40}", args.commit) is not None)
        self.private = args.run.resolve()
        require(not self.private.exists())
        self.private.mkdir()
        self.ui, self.temp = self.private / "ui", self.private / "setup-temp"
        self.ui.mkdir(); self.temp.mkdir(); (self.private / "runtime").mkdir()
        prefs = self.private / "preferences"
        prefs.mkdir()
        self.preferences = prefs / "app-preferences.json"
        shutil.copyfile(args.source / "fixtures/gui-sdk-preparation/app-preferences.json", self.preferences)
        preference_policy = self.strict_json("preferences_json", self.preferences)
        require(preference_policy["load_profile_at_startup"] is False and
                preference_policy["save_profile_on_exit"] is False)
        preference_policy["load_profile_at_startup"] = True
        self.preferences.write_text(json.dumps(preference_policy, indent=2) + "\n", encoding="utf-8")
        self.preferences_initial = self.preferences.read_bytes()
        profile = self.private / "hosted-profile.json"
        profile.write_text(json.dumps({"version": 1, "concurrent": True,
            "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": []},
            "seats": [{"id": "hosted-virtual-1", "name": "Hosted virtual user",
                       "mouse": "virtual://mouse/1", "keyboard": "virtual://keyboard/1"}]}), encoding="utf-8")
        self.profile_source, self.profile_initial = profile, profile.read_bytes()
        self.steps = self.strict_json("prefix_json", args.source / "fixtures/gui-sdk-hosted-mode/prepare.json")
        require(len(self.steps) == 36)
        shutil.copyfile(args.source / "fixtures/gui-sdk-hosted-mode/prepare.json", self.ui / "prepare.json")
        gui_root = args.artifacts / "sdk-preview/gui"
        gui_meta = self.strict_json("artifact_json", gui_root / "metadata.json")
        require(gui_meta["source_commit"] == args.commit and gui_meta["features"] == ["software-renderer"])
        self.exe = self.checked(gui_root, gui_meta["executable"], "protoinputmultiplex.exe")
        native_root = args.artifacts / "sdk-preview/x64"
        self.native_meta = self.strict_json("artifact_json", native_root / "metadata.json")
        require(self.native_meta["source_commit"] == args.commit and
                self.native_meta["native_client_abi_version"] == 3)
        self.native_exe = self.checked(native_root, self.native_meta["external_fixture"], "native_external_consumer.exe")
        self.dll = self.checked(native_root, self.native_meta["dll"], "mux_application_sdk.dll")
        web_root = args.artifacts / "application-web"
        web_meta = self.strict_json("artifact_json", web_root / "metadata.json")
        require(web_meta["source_commit"] == args.commit and web_meta["rust_wasm"] is True)
        require({entry["file"] for entry in web_meta["files"]} ==
                {"index.html", "bootstrap.js", "pkg/mux_application_web.js", "pkg/mux_application_web_bg.wasm"})
        public = self.private / "static"
        public.mkdir(); (public / "pkg").mkdir()
        for entry in web_meta["files"]:
            source = self.checked(web_root, entry, entry["file"])
            target = public / entry["file"]
            shutil.copyfile(source, target)
        shutil.copyfile(args.source / "crates/mux-application-web/example/hosted_mode_acceptance.js", public / "hosted_mode_acceptance.js")
        (public / "hosted.js").write_text("import {client} from './bootstrap.js'; import {runHostedModeAcceptance,runHostedLostReceipt,installHostedReplyFilter} from './hosted_mode_acceptance.js'; window.pimClient=client; installHostedReplyFilter(); window.runHostedModeAcceptance=runHostedModeAcceptance; window.runHostedLostReceipt=runHostedLostReceipt;", encoding="utf-8")
        (public / "hosted.html").write_text('<!doctype html><div id="application-demo"></div><script type="module" src="./hosted.js"></script>', encoding="utf-8")
        class Handler(http.server.SimpleHTTPRequestHandler):
            def __init__(self, *values, **kwargs):
                super().__init__(*values, directory=str(public), **kwargs)
            def log_message(self, *values):
                pass
        self.http = base.StaticServer(("127.0.0.1", 4173), Handler)
        threading.Thread(target=self.http.serve_forever, daemon=True).start()
        self.stage = "gui_preparation"
        env = dict(os.environ, TEMP=str(self.temp), TMP=str(self.temp))
        self.gui = self.launch([self.exe, "gui", "--renderer", "software-dx12", "--profile", profile,
            "--preferences", self.preferences, "--ui-script", self.ui / "prepare.json",
            "--ui-script-no-screenshots"], "gui", env)
        self.seen_descriptors, self.connection_paths = set(), {}
        for index, step in enumerate(self.steps, 1):
            self.checkpoint = index
            self.wait(lambda: len(self.receipts()) >= index)
            rows = self.receipts()
            require(rows[index - 1]["button"] == step["button"] and rows[index - 1]["clicked"] is True)
            if step.get("wait_for_sample"):
                self.wait(lambda: self.sample(index), 40)
                self.acknowledge(index)
        self.stage = "mcp_native_stop_restart"
        self.mcp_acceptance()
        self.mcp.stdin.close()
        require(self.mcp.wait(timeout=8) == 0)
        self.cdp.call("Browser.close")
        require(self.browser.wait(timeout=8) == 0)
        self.http.shutdown(); self.http.server_close()
        self.stage = "ordinary_quit"
        self.close_gui()
        self.result["ordinary_quit_clean"] = True
        require(self.profile_source.read_bytes() == self.profile_initial)
        self.result["profile_file_unchanged"] = True
        require(self.preferences.read_bytes() == self.preferences_initial)
        self.result["preferences_file_unchanged"] = True
        require(self.result["native_viewport_receipts"] == 2 and self.result["native_completed"] and
                self.result["native_retired"] and self.result["web_mode_worker_receipts"] == 2 and self.result["web_viewport_receipts"] == 2 and
                self.result["mcp_mode_worker_receipts"] == 2 and self.result["old_revision_rejected"] and
                self.result["old_epoch_rejected"] and
                self.result["event_epoch_retained"] and self.result["routing_generation_advanced"] and
                self.result["pending_refresh_correlated"] and self.result["ordinary_quit_clean"] and
                self.result["profile_file_unchanged"] and self.result["preferences_file_unchanged"] and self.result["lost_receipt_deadline"] and
                self.result["fixture_injected_gap"] and self.result["browser_no_replay"] and
                self.result["stale_timer_reconnect_isolated"])
        self.result["status"] = "PASS"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--run", type=Path, required=True)
    parser.add_argument("--public-result", type=Path, required=True)
    parser.add_argument("--commit", required=True)
    args = parser.parse_args()
    pair = HostedModePair(args)
    try:
        pair.run()
    except BaseException as error:
        if pair.checkpoint == 19 and hasattr(pair, "native"):
            pair.native_retirement_diagnostic()
        pair.result["failure_stage"] = pair.stage
        pair.result["checkpoint"] = pair.checkpoint
        pair.result["failure_class"] = type(error).__name__ if type(error).__name__ in {
            "Rejected", "KeyError", "TypeError", "ValueError", "JSONDecodeError",
            "FileNotFoundError", "PermissionError", "TimeoutError", "TimeoutExpired",
            "ConnectionError", "ConnectionResetError", "OSError", "Empty"} else "UnexpectedError"
        pair.result["failure_policy"] = "private partial evidence preserved; no kill, restart, or replay"
    pair.result["elapsed_ms"] = int((time.monotonic() - pair.start) * 1000)
    require(not args.public_result.exists())
    args.public_result.write_text(json.dumps(pair.result, indent=2), encoding="utf-8")
    return 0 if pair.result["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
