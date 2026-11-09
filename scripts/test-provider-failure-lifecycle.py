"""Inert failure cleanup and log-classification regressions; hosted execution only."""
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("pim_provider_failure_test",
    Path(__file__).with_name("qualify-gui-desktop-provider.py"))
provider = importlib.util.module_from_spec(spec)
spec.loader.exec_module(provider)


class FakeGui:
    def __init__(self, code=None, wait_error=None):
        self.code = code
        self.wait_error = wait_error
        self.waits = []

    def poll(self):
        return self.code

    def wait(self, timeout):
        self.waits.append(timeout)
        if self.wait_error:
            raise self.wait_error
        self.code = 0
        return self.code


class Gate(provider.ProviderPair):
    def __init__(self, gui):
        self.gui = gui
        self.end = 25
        self.live_read_retries = 0
        self.private = Path("private-not-opened")
        self.qualification_stderr_opt_in = True
        self.close_calls = 0
        self.close_error = None
        self.result = {"status":"FAIL","checkpoint":11,"failure_stage":"prepare_holds",
            "failure_predicate":"prepare_holds_target_held_state","failure_class":"Rejected"}

    def close_gui_owned_main(self):
        self.close_calls += 1
        self.result["gui_close_probe"] = {key:True for key in (
            "owner_pid_matches", "owner_thread_nonzero", "recheck_same_window", "recheck_same_thread",
            "recheck_owner_pid_matches", "immediate_eligibility_rechecked", "immediate_same_thread",
            "immediate_owner_pid_matches", "gui_alive_before_post", "close_posted")}
        if self.close_error:
            self.result["gui_close_probe"]["close_posted"] = None
            self.result["gui_close_probe"]["immediate_owner_pid_matches"] = False
            raise self.close_error


class FailureLifecycleTests(unittest.TestCase):
    def test_no_gui_never_attempts_close(self):
        gate = Gate(None)
        gate.failed_group_cleanup()
        self.assertEqual(gate.close_calls, 0)
        self.assertEqual(gate.result["failure_cleanup"]["outcome"], "gui_not_launched")
        self.assertFalse(gate.result["failure_cleanup"]["gui_exit_observed"])

    def test_cleanup_freezes_original_failure_and_uses_remaining_deadline(self):
        gate = Gate(FakeGui())
        original = dict(gate.result)
        with patch.object(provider.time, "monotonic", return_value=10):
            gate.failed_group_cleanup()
        self.assertEqual(gate.close_calls, 1)
        self.assertEqual(gate.gui.waits, [15])
        self.assertEqual({key:gate.result[key] for key in original}, original)
        cleanup = gate.result["failure_cleanup"]
        self.assertTrue(cleanup["close_identity_verified"])
        self.assertTrue(cleanup["gui_exit_observed"])
        self.assertTrue(cleanup["gui_exit_zero"])
        self.assertEqual(cleanup["target_holds"], "unobserved")

    def test_failed_identity_does_not_retry_or_wait(self):
        gate = Gate(FakeGui())
        gate.close_error = provider.base.Rejected("private secret path")
        with patch.object(provider.time, "monotonic", return_value=10):
            gate.failed_group_cleanup()
        self.assertEqual(gate.close_calls, 1)
        self.assertEqual(gate.gui.waits, [])
        cleanup = gate.result["failure_cleanup"]
        self.assertFalse(cleanup["close_identity_verified"])
        self.assertIsNone(cleanup["close_posted"])
        self.assertFalse(cleanup["gui_exit_observed"])
        self.assertNotIn("private secret path", repr(cleanup))

    def test_existing_close_probe_prevents_second_close(self):
        gate = Gate(FakeGui())
        gate.result["gui_close_probe"] = {"close_posted":True}
        gate.failed_group_cleanup()
        self.assertEqual(gate.close_calls, 0)
        self.assertEqual(gate.gui.waits, [])
        self.assertEqual(gate.result["failure_cleanup"]["outcome"], "prior_close_unconfirmed")

    def test_expired_deadline_never_posts_close(self):
        gate = Gate(FakeGui())
        with patch.object(provider.time, "monotonic", return_value=25):
            gate.failed_group_cleanup()
        self.assertEqual(gate.close_calls, 0)
        self.assertFalse(gate.result["failure_cleanup"]["gui_exit_observed"])

    def test_timeout_does_not_become_exit_receipt(self):
        gate = Gate(FakeGui(wait_error=subprocess.TimeoutExpired("private secret path", 15)))
        with patch.object(provider.time, "monotonic", return_value=10):
            gate.failed_group_cleanup()
        cleanup = gate.result["failure_cleanup"]
        self.assertEqual(gate.close_calls, 1)
        self.assertEqual(gate.gui.waits, [15])
        self.assertTrue(cleanup["close_posted"])
        self.assertFalse(cleanup["gui_exit_observed"])
        self.assertIsNone(cleanup["gui_exit_zero"])
        self.assertEqual(cleanup["failure_class"], "TimeoutExpired")
        self.assertNotIn("private secret path", repr(cleanup))


class DirectReadinessTests(unittest.TestCase):
    def record(self, target=b"target_a", state=b"Prepared", error=b"None"):
        return (b"PIM_QUAL_READY_SHUTDOWN_V1 phase=after_existing_shutdown_attempt_before_worker_terminal target="+target+
            b" status=Observed present=Some(true) coherent=Some(true) header_matches=Some(true) "
            b"identity_matches=Some(true) build_matches=Some(true) binding_present=Some(true) state=Some("+state+
            b") error=Some("+error+b")\n")

    def test_direct_records_are_fixed_sanitized_facts_and_reject_near_misses(self):
        record = self.record()
        result = provider.direct_ready_lines(record+self.record(b"target_b", b"Poisoned", b"Unknown"))
        self.assertEqual(result["target_a"]["state"], "prepared")
        self.assertEqual(result["target_b"]["error"], "unknown")
        self.assertFalse(result["malformed_record"] or result["duplicate_role"])
        for bad in (record[:-1], b"private-prefix "+record,
                record.replace(b"present=Some(true)", b"present=1"),
                record.replace(b"identity_matches=Some(true)", b"identity_matches=Some(false)"),
                record.replace(b"error=Some(None)", b"error=Some(0xC0000001)"),
                record.replace(b"target=target_a", b"target=private_pid_123"),
                record.replace(provider.DIRECT_READY_PHASE, b"after_gui_exit"),
                record[:-1]+b" private_path=secret\n"):
            rejected = provider.direct_ready_lines(bad)
            self.assertEqual(rejected["target_a"]["status"], "unobserved")
            self.assertNotIn("secret", repr(rejected))
            self.assertNotIn("0xC000", repr(rejected))
            self.assertNotIn("123", repr(rejected))
        duplicate = provider.direct_ready_lines(record+self.record(state=b"Poisoned"))
        self.assertTrue(duplicate["duplicate_role"])
        self.assertEqual(duplicate["target_a"]["state"], "prepared")

    def test_late_collection_is_once_after_verified_exit_and_keeps_prior_facts(self):
        gate = Gate(FakeGui())
        gate.result["native_submission_diagnostics"] = {"collection_phase":"before_failure_cleanup"}
        original = json.loads(json.dumps(gate.result))
        opened = []
        def open_log(path, mode):
            self.assertEqual(gate.gui.code, 0)
            self.assertEqual(gate.close_calls, 1)
            self.assertEqual(path.name, "gui-stderr.log")
            self.assertEqual(mode, "rb")
            opened.append(path.name)
            return io.BytesIO(self.record()+self.record(b"target_b", b"Poisoned", b"Unknown"))
        with patch.object(provider.time, "monotonic", return_value=10), \
                patch.object(provider.time, "sleep", side_effect=AssertionError("no new wait")), \
                patch.object(Path, "open", autospec=True, side_effect=open_log):
            gate.failed_group_cleanup()
            gate.late_direct_readiness()
        self.assertEqual(opened, ["gui-stderr.log"])
        self.assertEqual({key:gate.result[key] for key in original}, original)
        result = gate.result["late_direct_native_readiness"]
        self.assertEqual(result["producer_phase"], provider.DIRECT_READY_PHASE.decode("ascii"))
        self.assertEqual(result["collection_phase"], "bounded_stderr_read_after_verified_gui_exit")
        self.assertEqual(result["target_b"]["state"], "poisoned")
        self.assertFalse(result["stream_eof_proved"])
        self.assertFalse(result["truncated"] or result["incomplete_line"])

    def test_late_collection_truncation_partial_tail_and_read_failure_remain_inconclusive(self):
        for raw, limit in ((self.record(), 64), (self.record()[:-1], provider.NATIVE_STDERR_LIMIT)):
            gate = Gate(FakeGui(0))
            with patch.object(provider, "NATIVE_STDERR_LIMIT", limit), \
                    patch.object(Path, "open", return_value=io.BytesIO(raw)):
                gate.late_direct_readiness()
            result = gate.result["late_direct_native_readiness"]
            self.assertEqual(result["target_a"]["status"], "unobserved")
            self.assertTrue(result["truncated"] or result["incomplete_line"])
        gate = Gate(FakeGui(0))
        with patch.object(Path, "open", side_effect=OSError("private secret 123")):
            gate.late_direct_readiness()
        result = gate.result["late_direct_native_readiness"]
        self.assertIsNotNone(result["failure_class"])
        self.assertNotIn("secret", repr(result))
        self.assertNotIn("123", repr(result))


class ReportRejectionReasonTests(unittest.TestCase):
    def gate(self):
        gate = Gate(FakeGui())
        gate.stage, gate.check = "prepare_holds", "prepare_holds_target_held_state"
        return gate

    def test_existing_age_check_and_nested_or_held_expiry_have_distinct_fixed_reasons(self):
        gate = self.gate()
        with patch.object(provider.time, "monotonic", return_value=0), \
                patch.object(provider.time, "time", return_value=3), \
                patch.object(provider.base, "shared_live_bytes", return_value=(b"{}", 0)) as reads, \
                patch.object(provider.time, "sleep", side_effect=AssertionError("age rejects immediately")):
            with self.assertRaises(provider.base.Rejected):
                gate.target(0)
        self.assertEqual(reads.call_count, 1)
        self.assertEqual(gate.result["prepare_holds_failure_reason"], "report_age_rejected")
        gate = self.gate()
        with patch.object(provider.time, "monotonic", side_effect=[0, 4]):
            with self.assertRaises(provider.base.Rejected):
                gate.target(0)
        self.assertEqual(gate.result["prepare_holds_failure_reason"], "target_report_wait_expired")
        gate = self.gate()
        with patch.object(provider.time, "monotonic", side_effect=[0, 16]):
            with self.assertRaises(provider.base.Rejected):
                gate.wait(lambda: False, on_expired=lambda: gate.prepare_holds_reason("held_wait_expired"))
        self.assertEqual(gate.result["prepare_holds_failure_reason"], "held_wait_expired")

    def test_expiry_callback_failure_cannot_replace_original_rejection_or_result(self):
        gate = self.gate()
        def broken():
            raise RuntimeError("private diagnostic failure")
        with patch.object(provider.time, "monotonic", side_effect=[0, 16]):
            with self.assertRaises(provider.base.Rejected) as rejected:
                gate.wait(lambda: False, on_expired=broken)
        self.assertEqual(rejected.exception.args, ())
        result = object()
        with patch.object(provider.time, "monotonic", return_value=0):
            self.assertIs(gate.wait(lambda: result, on_expired=broken), result)
        error = ValueError("original predicate error")
        def failed():
            raise error
        with patch.object(provider.time, "monotonic", return_value=0):
            with self.assertRaises(ValueError) as original:
                gate.wait(failed, on_expired=broken)
        self.assertIs(original.exception, error)
        gate.prepare_holds_reason("private unrecognized reason")
        self.assertNotIn("prepare_holds_failure_reason", gate.result)
        class BrokenDiagnosticDict(dict):
            def __setitem__(self, key, value):
                if key == "prepare_holds_failure_reason":
                    raise RuntimeError("private diagnostic store failure")
                super().__setitem__(key, value)
        gate.result = BrokenDiagnosticDict(gate.result)
        with patch.object(provider.time, "monotonic", return_value=0), \
                patch.object(provider.time, "time", return_value=3), \
                patch.object(provider.base, "shared_live_bytes", return_value=(b"{}", 0)):
            with self.assertRaises(provider.base.Rejected):
                gate.target(0)


class PostCloseNativeLifecycleTests(unittest.TestCase):
    def lifecycle(self, state="poisoned", error="unknown", binding=True):
        return {"status":"observed","present":True,"coherent":True,"header_matches":True,
            "identity_matches":True,"build_matches":True,"binding_present":binding,"state":state,"error":error}

    def gate(self):
        gate = Gate(FakeGui())
        gate.targets_processes = [object(), object()]
        gate.failure_target_frames = {0:10, 1:20}
        return gate

    def test_verified_close_then_exactly_one_read_each_preserves_original_failure(self):
        gate = self.gate()
        gate.result["prepare_holds_observation"] = {"rightctrl_held":False,"mapped_x1_held":True}
        gate.result["prepare_holds_router"] = {"recipient_matches":True}
        original = json.loads(json.dumps(gate.result))
        calls = []
        def read(path, maximum):
            self.assertEqual(gate.gui.code, 0)
            self.assertEqual(gate.close_calls, 1)
            self.assertEqual({key:gate.result[key] for key in original}, original)
            self.assertEqual(maximum, 4*1024*1024)
            calls.append(path.name)
            index = len(calls)-1
            native = self.lifecycle() if index == 0 else self.lifecycle("reusable", "none", False)
            return json.dumps({"frames":11+10*index,"native_lifecycle":native,
                "private_path":"secret","pid":123}).encode(), 0
        with patch.object(provider.time, "monotonic", return_value=10), \
                patch.object(provider.time, "sleep", side_effect=AssertionError("no new wait")), \
                patch.object(gate, "target", side_effect=AssertionError("target waits")), \
                patch.object(provider.base, "shared_live_bytes", side_effect=read):
            gate.failed_group_cleanup()
            gate.post_close_target_reports()  # The same observation cannot be repeated.
        self.assertEqual(calls, ["target-0.json", "target-1.json"])
        self.assertEqual(gate.gui.waits, [15])
        result = gate.result["post_close_native_lifecycle"]
        self.assertEqual(result["collection_phase"], "report_read_after_gui_exit")
        self.assertFalse(result["periodic_sample_after_gui_exit_proved"])
        self.assertEqual(result["target_a"]["native_lifecycle"], self.lifecycle())
        self.assertEqual(result["target_b"]["native_lifecycle"], self.lifecycle("reusable", "none", False))
        self.assertTrue(result["target_a"]["advanced_since_failure_sample"])
        self.assertEqual({key:gate.result[key] for key in original}, original)
        self.assertNotIn("secret", repr(result))
        self.assertNotIn("123", repr(result))

    def test_partial_and_stale_reports_are_unobserved_without_retry(self):
        gate = self.gate()
        stale = json.dumps({"frames":20,"native_lifecycle":self.lifecycle("active", "none")}).encode()
        with patch.object(provider.time, "monotonic", return_value=10), \
                patch.object(provider.base, "shared_live_bytes", side_effect=[(b'{"private":', 0), (stale, 0)]) as reads:
            gate.failed_group_cleanup()
        self.assertEqual(reads.call_count, 2)
        result = gate.result["post_close_native_lifecycle"]
        self.assertEqual(result["target_a"]["read_status"], "read_failed")
        self.assertEqual(result["target_a"]["failure_class"], "JSONDecodeError")
        self.assertEqual(result["target_b"]["read_status"], "stale")
        self.assertFalse(result["target_b"]["advanced_since_failure_sample"])
        for label in ("target_a", "target_b"):
            self.assertEqual(result[label]["native_lifecycle"], provider.public_native_lifecycle({}))

    def test_absent_unknown_and_missing_freshness_never_export_private_data(self):
        for first, second in (
            (FileNotFoundError("private secret"), {"frames":21,"native_lifecycle":dict(self.lifecycle(), owner_pid=123)}),
            ({"frames":True,"native_lifecycle":self.lifecycle()}, {"native_lifecycle":self.lifecycle()}),
        ):
            gate = self.gate()
            values = [value if isinstance(value, BaseException) else (json.dumps(value).encode(), 0)
                for value in (first, second)]
            with patch.object(provider.time, "monotonic", return_value=10), \
                    patch.object(provider.base, "shared_live_bytes", side_effect=values) as reads:
                gate.failed_group_cleanup()
            self.assertEqual(reads.call_count, 2)
            result = gate.result["post_close_native_lifecycle"]
            for label in ("target_a", "target_b"):
                self.assertIn(result[label]["native_lifecycle"]["status"], ("unobserved", "unrecognized"))
                self.assertIsNone(result[label]["native_lifecycle"]["state"])
            self.assertNotIn("secret", repr(result))
            self.assertNotIn("123", repr(result))

    def test_no_verified_exit_or_owned_targets_never_reads(self):
        for gui, bad_identity, owned in ((None, False, True),
                (FakeGui(wait_error=subprocess.TimeoutExpired("private", 15)), False, True),
                (FakeGui(), True, True), (FakeGui(), False, False)):
            gate = self.gate()
            gate.gui = gui
            if bad_identity:
                gate.close_error = provider.base.Rejected("private")
            if not owned:
                gate.targets_processes = []
            with patch.object(provider.time, "monotonic", return_value=10), \
                    patch.object(provider.base, "shared_live_bytes", side_effect=AssertionError("not owned/exited")) as reads:
                gate.failed_group_cleanup()
            reads.assert_not_called()
            self.assertEqual(gate.result["status"], "FAIL")


class NativeStderrTests(unittest.TestCase):
    def test_only_complete_exact_fixed_lines_match(self):
        raw = (b"private path token\nFailed to send message header\r\n"
            b"prefix Failed to send message body\nFailed to send message body suffix\n"
            b"Failed to send message body")
        self.assertEqual(provider.native_write_failure_lines(raw),
            {"header_write_failed":True,"body_write_failed":False})
        self.assertTrue(provider.native_write_failure_lines(raw+b"\n")["body_write_failed"])

    def snapshot(self, gate, raw):
        source = io.BytesIO(raw)
        with patch.object(Path, "open", return_value=source):
            gate.native_submission_diagnostics()
        return gate.result["native_submission_diagnostics"]

    def test_live_log_is_not_complete(self):
        result = self.snapshot(Gate(FakeGui()), b"Failed to send message body\n")
        self.assertTrue(result["body_write_failed"])
        self.assertFalse(result["snapshot_complete"])

    def test_exited_bounded_log_without_partial_tail_is_complete(self):
        result = self.snapshot(Gate(FakeGui(0)), b"")
        self.assertTrue(result["snapshot_complete"])
        self.assertFalse(result["header_write_failed"])
        self.assertFalse(result["body_write_failed"])

    def test_truncation_and_partial_tail_are_incomplete(self):
        with patch.object(provider, "NATIVE_STDERR_LIMIT", 64):
            result = self.snapshot(Gate(FakeGui(0)), b"Failed to send message header\n"+b"private"*20)
        self.assertTrue(result["header_write_failed"])
        self.assertTrue(result["truncated"])
        self.assertTrue(result["incomplete_line"])
        self.assertFalse(result["snapshot_complete"])
        self.assertNotIn("private", repr(result))

    def test_read_error_is_sanitized_and_not_negative_evidence(self):
        gate = Gate(FakeGui(0))
        with patch.object(Path, "open", side_effect=OSError("private secret path")):
            gate.native_submission_diagnostics()
        result = gate.result["native_submission_diagnostics"]
        self.assertIsNone(result["header_write_failed"])
        self.assertIsNone(result["body_write_failed"])
        self.assertFalse(result["snapshot_complete"])
        self.assertEqual(result["read_failure"]["read_role"], "gui_stderr")
        self.assertNotIn("private secret path", repr(result))


class NativeKeyCallBoundaryTests(unittest.TestCase):
    route = (b"PIM_QUAL_KEY_ROUTE_V1 decision=submit seat_keyboard_matches=true "
        b"independent_keyboards_requested=true keyboard_enabled=true "
        b"recipient_matches_clicked=true packet_matches_fixture=true\n")
    context = (b"PIM_QUAL_KEY_CONTEXT_V1 target_identity_matches=true recipient_pid_matches=true "
        b"recipient_window_matches=true selected_window_matches=true instance_binding_matches=true "
        b"startup_config_calls_returned=true requested_keyboard_messages=true "
        b"packet_keyboard_assigned=true requested_frozen=false native_backend_selected=true\n")
    entered = (b"PIM_QUAL_KEY_CALL_V1 phase=entered instance_nonzero=true device_matches=true "
        b"kind_matches=true flags_match=true scan_matches=true y_matches=true key_matches=true\n")
    returned = entered.replace(b"phase=entered", b"phase=returned")

    def test_decision_context_and_void_call_are_distinct_ordered_facts(self):
        raw = self.route+self.context+self.entered+self.returned
        result = provider.native_key_call_lines(raw)
        self.assertEqual(result["route"]["decision"], "submit")
        self.assertTrue(result["context"]["startup_config_calls_returned"])
        self.assertFalse(result["context"]["requested_frozen"])
        self.assertTrue(result["call_entered"]["key_matches"])
        self.assertTrue(result["call_returned"]["key_matches"])
        self.assertTrue(result["order_valid"])
        self.assertFalse(result["malformed_record"] or result["duplicate_record"])
        self.assertNotIn("accepted", repr(result))
        snapshot = NativeStderrTests().snapshot(Gate(FakeGui()), raw)
        self.assertEqual(snapshot["first_key_down"], result)
        self.assertFalse(snapshot["snapshot_complete"])

    def test_suppression_and_incomplete_call_never_invent_completion(self):
        for decision in (b"duplicate_hold", b"shared_hold"):
            result = provider.native_key_call_lines(self.route.replace(b"decision=submit", b"decision="+decision))
            self.assertEqual(result["route"]["decision"], decision.decode("ascii"))
            self.assertIsNone(result["context"])
            self.assertIsNone(result["call_entered"])
            self.assertIsNone(result["call_returned"])
        result = provider.native_key_call_lines(self.route+self.context+self.entered+self.returned[:-1])
        self.assertIsNotNone(result["call_entered"])
        self.assertIsNone(result["call_returned"])
        mismatch = provider.native_key_call_lines(self.entered.replace(b"key_matches=true", b"key_matches=false"))
        self.assertFalse(mismatch["call_entered"]["key_matches"])

    def test_near_miss_private_fields_and_invalid_values_are_rejected(self):
        for raw in (
            b"private-prefix "+self.entered,
            self.entered.replace(b"key_matches=true", b"key_matches=1"),
            self.entered.replace(b"phase=entered", b"phase=accepted"),
            self.entered.replace(b"key_matches=true", b"private_path=secret"),
            self.entered[:-1]+b" private_pid=123\n",
        ):
            result = provider.native_key_call_lines(raw)
            self.assertIsNone(result["call_entered"])
            self.assertNotIn("secret", repr(result))
            self.assertNotIn("123", repr(result))
        out_of_order = provider.native_key_call_lines(self.returned+self.entered)
        self.assertFalse(out_of_order["order_valid"])
        repeated = provider.native_key_call_lines(self.entered+self.entered)
        self.assertTrue(repeated["duplicate_record"])


class NativeLifecycleBoundaryTests(unittest.TestCase):
    def test_lifecycle_whitelist_preserves_uncertainty_and_rejects_private_or_malformed_values(self):
        observed = {"status":"observed","present":True,"coherent":True,"header_matches":True,
            "identity_matches":True,"build_matches":True,"binding_present":True,
            "state":"poisoned","error":"broken_pipe"}
        self.assertEqual(provider.public_native_lifecycle({"native_lifecycle":observed}), observed)
        self.assertEqual(provider.public_native_lifecycle({})["status"], "unobserved")
        for status, present, coherent in (("absent", False, None), ("unstable", True, False),
                ("read_failed", None, None), ("invalid_identity", True, True)):
            row = dict.fromkeys(observed)
            row.update(status=status, present=present, coherent=coherent)
            self.assertEqual(provider.public_native_lifecycle({"native_lifecycle":row})["status"], status)
        rejected = [dict(observed, owner_pid="private secret path"), dict(observed, error=109),
            dict(observed, state=["active"]), dict(observed, status={"private":"secret"}),
            dict(observed, identity_matches=False), dict(observed, present=1),
            dict(observed, status="invalid_identity"), {"status":"observed"}]
        for row in rejected:
            result = provider.public_native_lifecycle({"native_lifecycle":row})
            self.assertEqual(result["status"], "unrecognized")
            self.assertTrue(all(value is None for key, value in result.items() if key != "status"))


class MainObservationOrderTests(unittest.TestCase):
    def test_main_collects_once_after_failure_freeze_before_cleanup_or_after_success(self):
        for fail in (True, False):
            with self.subTest(fail=fail), tempfile.TemporaryDirectory() as directory:
                trace, samples = [], []

                class MainGate:
                    def __init__(self, args):
                        self.result = {"status":"FAIL"}
                        self.checkpoint, self.stage, self.check = 11, "prepare_holds", "prepare_holds_target_held_state"

                    def run(self):
                        trace.append("run")
                        if fail:
                            raise provider.base.Rejected("private secret path")
                        self.result["status"] = "PASS"

                    def native_submission_diagnostics(self, collection_phase="direct_observation"):
                        trace.append(collection_phase)
                        samples.append(dict(self.result))

                    def failed_group_cleanup(self):
                        trace.append("cleanup")
                        self.checkpoint, self.stage, self.check = 99, "cleanup", "cleanup"
                        self.result["failure_cleanup"] = {"outcome":"gui_exit_observed"}

                root = Path(directory)
                result_path = root / "public-result.json"
                argv = ["qualify-gui-desktop-provider.py"]
                for name in ("artifacts", "backend", "source", "run"):
                    argv.extend(["--"+name, str(root / name)])
                argv.extend(["--public-result", str(result_path), "--commit", "0"*40])
                with patch.object(provider, "ProviderPair", MainGate), patch.object(sys, "argv", argv):
                    with self.assertRaises(SystemExit) as exited:
                        provider.main()
                self.assertEqual(exited.exception.code, 1 if fail else 0)
                self.assertEqual(trace, ["run", "before_failure_cleanup", "cleanup"] if fail else ["run", "after_run"])
                self.assertEqual(len(samples), 1)
                result = json.loads(result_path.read_text(encoding="utf-8"))
                if fail:
                    frozen = {"status":"FAIL","checkpoint":11,"failure_stage":"prepare_holds",
                        "failure_predicate":"prepare_holds_target_held_state","failure_class":"Rejected",
                        "native_exit_state":"not_started"}
                    for key, value in frozen.items():
                        self.assertEqual(samples[0][key], value)
                        self.assertEqual(result[key], value)
                    self.assertNotIn("private secret path", repr(result))
                else:
                    self.assertEqual(samples[0]["status"], "PASS")
                    self.assertNotIn("failure_cleanup", result)


if __name__ == "__main__":
    unittest.main()
