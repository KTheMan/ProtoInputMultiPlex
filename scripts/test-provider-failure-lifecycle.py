"""Inert failure cleanup and log-classification regressions; hosted execution only."""
import importlib.util
import io
from pathlib import Path
import subprocess
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


if __name__ == "__main__":
    unittest.main()
