"""Inert failure cleanup and log-classification regressions; hosted execution only."""
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
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


class CrashEventTests(unittest.TestCase):
    image = r"D:\private\protoinputmultiplex.exe"
    module = r"D:\private\runtime\ProtoInputHooks64.dll"
    creation = 132223104000000000

    def record(self, **changes):
        values = {"ProcessId":"0x2a","ProcessCreationTime":hex(self.creation),"AppPath":self.image,
            "ModulePath":self.module,"ModuleName":"ProtoInputHooks64.dll","ExceptionCode":"c0000005",
            "FaultingOffset":"0000000000001234"}
        values.update(changes)
        data = "".join('<Data Name="'+key+'">'+value+'</Data>' for key, value in values.items())
        return ('<Event xmlns="http://schemas.microsoft.com/win/2004/08/events/event"><System>'
            '<Provider Name="Application Error"/><EventID>1000</EventID><Level>2</Level><Channel>Application</Channel>'
            '<TimeCreated SystemTime="2020-01-01T00:00:01.0000000Z"/></System><EventData>'+data+'</EventData></Event>')

    def match(self, raw, **changes):
        helper = provider.crash_events
        args = {"identity":(42,self.creation),"image":self.image,"upper":self.creation+20000000,
            "modules":{helper.windows_path(self.module):"a"*64}}
        args.update(changes)
        return helper.match_events(raw, **args)

    def test_event_requires_exact_original_identity_image_and_time_with_private_payload(self):
        public, raw, malformed = self.match([self.record()])
        self.assertEqual(public["status"], "matched")
        self.assertEqual(public["module_class"], "protoinput_hooks")
        self.assertEqual(public["instruction_rva"], 0x1234)
        self.assertEqual(public["own_module_basename"], "protoinputhooks64.dll")
        self.assertEqual(public["admitted_module_sha256"], "a"*64)
        self.assertFalse(malformed)
        self.assertIn("private", raw)
        self.assertNotIn("private", repr(public))
        self.assertNotIn(str(self.creation), repr(public))
        for altered in (self.record(ProcessId="0x2b"),self.record(ProcessCreationTime=hex(self.creation+1)),
                self.record(ProcessId="0x2b").replace("<EventID>",'<Execution ProcessID="42"/><EventID>'),
                self.record(AppPath=r"D:\other\protoinputmultiplex.exe")):
            self.assertEqual(self.match([altered])[0]["status"], "no_matching_event")
        self.assertEqual(self.match([self.record()], upper=self.creation)[0]["status"], "no_matching_event")
        self.assertEqual(self.match([self.record()], identity=(42,self.creation+20000000))[0]["status"], "no_matching_event")
        self.assertEqual(provider.crash_events.system_filetime("2020-01-01T00:00:01.0000001Z"), self.creation+10000001)

    def test_bad_xml_duplicate_fields_absolute_offsets_and_ambiguous_records_are_suppressed(self):
        raw = self.record()
        bad = ["<!DOCTYPE Event [<!ENTITY x 'private'>]>"+raw,raw[:-8],
            raw.replace("1000</EventID>","1001</EventID>"),raw.replace('Name="Application Error"','Name="other"'),
            raw.replace("<Level>2</Level>","<Level>4</Level>"),raw.replace("Application</Channel>","Security</Channel>"),
            raw.replace("<System>","<System><EventID>1000</EventID>"),
            raw.replace("</EventData>",'<Data Name="ProcessId">0x2a</Data></EventData>'),
            self.record(ProcessId="0x100000000"),self.record(ProcessCreationTime="private"),
            "x"*(provider.crash_events.MAX_XML_BYTES+1)]
        for value in bad:
            public, private, malformed = self.match([value])
            self.assertEqual(public["status"], "no_matching_event")
            self.assertIsNone(private)
            self.assertTrue(malformed)
            self.assertNotIn("private", repr(public))
        public, private, _ = self.match([raw,raw])
        self.assertEqual(public["status"], "ambiguous")
        self.assertIsNone(public["instruction_rva"])
        self.assertIsNone(private)

    def test_matching_identity_with_unknown_module_or_absolute_offset_reports_precise_gap(self):
        for changes, reason in (({"ModulePath":"unknown"},"module_unavailable"),
                ({"FaultingOffset":"00007ff8deadbeef"},"offset_unavailable"),
                ({"ExceptionCode":"private"},"exception_unavailable")):
            public, private, malformed = self.match([self.record(**changes)])
            self.assertEqual(public["status"], "matched_unattributable")
            self.assertEqual(public["attribution_status"], reason)
            self.assertIsNone(public["instruction_rva"])
            self.assertIsNotNone(private)
            self.assertFalse(malformed)
            self.assertNotIn("deadbeef", repr(public))

    def test_module_hash_requires_fixed_name_and_exact_admitted_path_and_is_not_loaded_identity(self):
        for changes in ({"ModulePath":r"D:\other\ProtoInputHooks64.dll"},{"ModuleName":"private.dll"},
                {"ModulePath":r"D:\private\private.dll","ModuleName":"private.dll"}):
            public, _, _ = self.match([self.record(**changes)])
            self.assertEqual(public["module_class"], "other_module")
            self.assertIsNone(public["own_module_basename"])
            self.assertIsNone(public["admitted_module_sha256"])
            self.assertNotIn("private", repr(public))
        public, _, _ = self.match([self.record(ModulePath=r"C:\Windows\System32\ntdll.dll",ModuleName="ntdll.dll")])
        self.assertEqual(public["module_class"], "windows_system")
        self.assertIsNone(public["admitted_module_sha256"])
        self.assertEqual(public["symbolization"], "not_inspected")

    def test_creation_reads_original_popen_handle_once_without_pid_reopen(self):
        helper, calls = provider.crash_events, []
        class Times:
            def __call__(inner, handle, created, *rest):
                calls.append(handle.value)
                value = helper.ctypes.cast(created, helper.ctypes.POINTER(helper.wintypes.FILETIME)).contents
                value.dwHighDateTime, value.dwLowDateTime = self.creation >> 32, self.creation & 0xffffffff
                return 1
        kernel = SimpleNamespace(GetProcessTimes=Times())
        with patch.object(helper.ctypes, "WinDLL", create=True, return_value=kernel), \
                patch.object(helper.time, "monotonic", return_value=1):
            identity = helper.original_creation(SimpleNamespace(_handle=123,pid=42), 2)
        self.assertEqual(calls, [123])
        self.assertEqual(identity, (42,self.creation))

    def test_single_event_batch_uses_zero_timeout_and_closes_query_and_event_handles(self):
        helper, raw = provider.crash_events, self.record()
        cases = ("success","next_failure","oversize","render_failure","render_raise","invalid_count",
            "expiry_after_bind","expiry_after_query","expiry_after_next","expiry_after_sizing")
        for case in cases:
            with self.subTest(case=case):
                trace, clock = [], [1]
                def query(*args):
                    trace.append(("query",args[-1]))
                    if case == "expiry_after_query": clock[0] = 3
                    return 101
                def next_events(query, capacity, handles, timeout, flags, count):
                    trace.append(("next",capacity,timeout,flags))
                    handles[0] = 201
                    helper.ctypes.cast(count, helper.ctypes.POINTER(helper.wintypes.DWORD)).contents.value = 33 if case == "invalid_count" else 1
                    if case == "expiry_after_next": clock[0] = 3
                    return case != "next_failure"
                def render(context, handle, flags, size, buffer, used, properties):
                    trace.append(("render",size))
                    amount = helper.MAX_XML_BYTES+2 if case == "oversize" else (len(raw)+1)*helper.ctypes.sizeof(helper.ctypes.c_wchar)
                    helper.ctypes.cast(used, helper.ctypes.POINTER(helper.wintypes.DWORD)).contents.value = amount
                    if buffer is None:
                        if case == "expiry_after_sizing": clock[0] = 3
                        return 0
                    if case == "render_raise": raise RuntimeError("private render failure")
                    if case == "render_failure": return 0
                    buffer.value = raw
                    return 1
                class Call:
                    def __init__(self, fn): self.fn = fn
                    def __call__(self, *args): return self.fn(*args)
                api = SimpleNamespace(EvtQuery=Call(query),EvtNext=Call(next_events),EvtRender=Call(render),
                    EvtClose=Call(lambda handle: trace.append(("close",handle))))
                def load(*args, **kwargs):
                    if case == "expiry_after_bind": clock[0] = 3
                    return api
                with patch.object(helper.ctypes, "WinDLL", create=True, side_effect=load), \
                        patch.object(helper.ctypes, "get_last_error", create=True, return_value=122), \
                        patch.object(helper.time, "monotonic", side_effect=lambda: clock[0]):
                    if case == "render_raise":
                        with self.assertRaises(RuntimeError): helper.query_events(2)
                    else:
                        result, records = helper.query_events(2)
                        self.assertEqual(records, [raw] if case == "success" else [])
                        if case.startswith("expiry_"): self.assertEqual(result["query_status"], "deadline_exhausted")
                        if case in ("oversize","render_failure","expiry_after_next","expiry_after_sizing"):
                            self.assertTrue(result["render_incomplete"])
                before_next = case in ("expiry_after_bind","expiry_after_query")
                self.assertEqual([row for row in trace if row[0] == "next"], [] if before_next else [("next",32,0,0)])
                closes = [] if case == "expiry_after_bind" else [("close",101)] if before_next else [("close",201),("close",101)]
                self.assertEqual([row for row in trace if row[0] == "close"], closes)
                renders = [row for row in trace if row[0] == "render"]
                if case in ("expiry_after_bind","expiry_after_query","expiry_after_next","next_failure","invalid_count"):
                    self.assertEqual(renders, [])
                elif case in ("expiry_after_sizing","oversize"):
                    self.assertEqual(renders, [("render",0)])

    def test_metadata_deadline_after_binding_skips_getprocesstimes(self):
        helper, clock, calls = provider.crash_events, [1], []
        class Times:
            def __call__(self, *args): calls.append(args); raise AssertionError("expired metadata call")
        def load(*args, **kwargs):
            clock[0] = 3
            return SimpleNamespace(GetProcessTimes=Times())
        with patch.object(helper.ctypes, "WinDLL", create=True, side_effect=load), \
                patch.object(helper.time, "monotonic", side_effect=lambda: clock[0]):
            with self.assertRaises(TimeoutError): helper.original_creation(SimpleNamespace(_handle=123,pid=42), 2)
        self.assertEqual(calls, [])

    def test_query_deadline_and_missing_api_do_not_poll_or_expose_errors(self):
        helper = provider.crash_events
        with patch.object(helper.time, "monotonic", return_value=3), \
                patch.object(helper.ctypes, "WinDLL", create=True, side_effect=AssertionError("expired")) as load:
            result, records = helper.query_events(2)
        self.assertEqual(result["query_status"], "deadline_exhausted")
        self.assertEqual(records, [])
        self.assertEqual(load.call_count, 0)
        with patch.object(helper.time, "monotonic", return_value=1), \
                patch.object(helper.ctypes, "WinDLL", create=True, side_effect=OSError("private secret")) as load:
            result, records = helper.query_events(2)
        self.assertEqual(result["query_status"], "api_unavailable")
        self.assertEqual(load.call_count, 1)
        self.assertNotIn("private", repr(result))

    def test_controller_requires_host_ownership_and_original_identity_before_event_query(self):
        helper = provider.crash_events
        for mode in ("wrong_host","not_owned","identity_unavailable","expired"):
            gate = Gate(FakeGui())
            if mode != "not_owned": gate.targets_processes = [object(),object()]
            with patch.object(provider.os, "name", "nt"), \
                    patch.dict(provider.os.environ, {"GITHUB_ACTIONS":"true",
                        "RUNNER_ENVIRONMENT":"self-hosted" if mode == "wrong_host" else "github-hosted"}), \
                    patch.object(provider.time, "monotonic", return_value=30 if mode == "expired" else 1), \
                    patch.object(helper, "original_creation", side_effect=OSError("private secret")) as creation, \
                    patch.object(helper, "query_events", side_effect=AssertionError("no unbound query")) as query:
                gate.target_crash_event_diagnostics()
            self.assertEqual(query.call_count, 0)
            self.assertEqual(creation.call_count, 2 if mode == "identity_unavailable" else 0)
            self.assertNotIn("private secret", repr(gate.result))

    def test_controller_samples_first_two_after_cleanup_and_retains_only_matched_private_xml(self):
        gate, helper, reads, written = Gate(FakeGui()), provider.crash_events, [], []
        gate.exe, gate.targets_processes = Path(self.image), [object(),object(),object()]
        original = dict(gate.result)
        gate.crash_admitted_modules = {helper.windows_path(self.module):"a"*64}
        records = [self.record(),self.record(ProcessId="43",ProcessCreationTime=hex(self.creation+1))]
        def identity(child, end):
            self.assertEqual(end, gate.end)
            reads.append(child)
            return (42,self.creation) if child is gate.targets_processes[0] else (43,self.creation+1)
        def save(path, raw, encoding): written.append((path.name,raw)); return len(raw)
        with patch.object(provider.os, "name", "nt"), \
                patch.dict(provider.os.environ, {"GITHUB_ACTIONS":"true","RUNNER_ENVIRONMENT":"github-hosted"}), \
                patch.object(helper, "original_creation", side_effect=identity), \
                patch.object(helper, "query_events", return_value=({"query_status":"queried"},records)) as query, \
                patch.object(provider.time, "monotonic", return_value=1), \
                patch.object(provider.time, "time_ns", return_value=(self.creation+20000000-116444736000000000)*100), \
                patch.object(Path, "write_text", autospec=True, side_effect=save):
            gate.target_crash_event_diagnostics()
            gate.target_crash_event_diagnostics()
        self.assertEqual(reads, gate.targets_processes[:2])
        query.assert_called_once_with(gate.end)
        self.assertEqual([name for name, raw in written], ["target_a-matched-crash-event.xml","target_b-matched-crash-event.xml"])
        self.assertEqual({key:gate.result[key] for key in original}, original)
        public = gate.result["target_crash_events"]
        self.assertEqual(public["target_a"]["status"], "matched")
        self.assertFalse(public["loaded_module_hash_proved"])
        self.assertNotIn("private", repr({key:value for key,value in public.items() if key != "target_a" and key != "target_b"}))

    def test_controller_expiry_after_first_metadata_skips_second_target_and_query(self):
        gate, helper, clock, reads = Gate(FakeGui()), provider.crash_events, [1], []
        gate.targets_processes = [object(),object()]
        original = dict(gate.result)
        def identity(child, end):
            reads.append(child)
            self.assertEqual(end, gate.end)
            clock[0] = 30
            return 42, self.creation
        with patch.object(provider.os, "name", "nt"), \
                patch.dict(provider.os.environ, {"GITHUB_ACTIONS":"true","RUNNER_ENVIRONMENT":"github-hosted"}), \
                patch.object(provider.time, "monotonic", side_effect=lambda: clock[0]), \
                patch.object(helper, "original_creation", side_effect=identity), \
                patch.object(helper, "query_events", side_effect=AssertionError("expired query")) as query:
            gate.target_crash_event_diagnostics()
        self.assertEqual(reads, gate.targets_processes[:1])
        self.assertEqual(query.call_count, 0)
        self.assertEqual(gate.result["target_crash_events"]["query_status"], "deadline_exhausted")
        self.assertEqual({key:gate.result[key] for key in original}, original)


class TargetTerminationTests(unittest.TestCase):
    def test_fixed_exit_classes_reject_noninteger_private_values(self):
        for value, expected in ((None,"running"),(0,"zero"),(1,"one"),(2,"other_nonzero"),
                (0xc0000005,"access_violation"),(0xc0000005-0x100000000,"access_violation"),
                (0xc00000fd,"stack_overflow"),(0xc0000409,"fail_fast"),(0xe06d7363,"cpp_exception"),
                (0x40000015,"fatal_app_exit"),(0xc000001d,"illegal_instruction"),
                (0xc0000135,"dll_not_found"),(0xc0000142,"dll_initialization_failed")):
            self.assertEqual(provider.target_exit_class(value), expected)
        for value in (True,False,"private secret path",{},[],0x100000000,-0x80000001):
            self.assertEqual(provider.target_exit_class(value), "unobserved")

    def test_stderr_requires_complete_specific_headers_and_exports_no_content(self):
        raw = (b"Error: Write automation probe report\r\n\nCaused by:\nprivate secret path\n"
            b"Error: Create automation probe Unicode text editor\nError: Read automation probe message\n"
            b"thread 'main' (1234) panicked at private secret path:9:2:\n"
            b"called `Result::unwrap()` on an `Err` value: PoisonError { .. }\n"
            b"thread caused non-unwinding panic. aborting.\n")
        result = provider.target_stderr_lines(raw)
        self.assertTrue(all(result.values()))
        self.assertNotIn("private", repr(result))
        self.assertNotIn("1234", repr(result))
        for bad in (b"Error: Write automation probe report",b"prefix Error: Write automation probe report\n",
                b"Error: Write automation probe report private secret path\n",
                b"Error: Create automation probe window private secret path\n",
                b"thread 'private' panicked at secret\n",b"prefix thread 'main' panicked at secret\n"):
            self.assertFalse(any(provider.target_stderr_lines(bad).values()))

    def test_owned_targets_are_polled_and_fixed_logs_read_once_without_wait_or_clock(self):
        gate = Gate(FakeGui())
        original = dict(gate.result)
        polls, paths, sizes = [], [], []
        class Target:
            def __init__(self, index, code): self.index, self.code = index, code
            def poll(self): polls.append(self.index); return self.code
            def wait(self, *args, **kwargs): raise AssertionError("no target wait")
        class Log(io.BytesIO):
            def read(self, size): sizes.append(size); return super().read(size)
        gate.targets_processes = [Target(0,None),Target(1,0xc0000005),Target(2,0)]
        def open_log(path, mode):
            self.assertEqual(mode, "rb")
            paths.append(path.name)
            return Log(b"Error: Write automation probe report\n")
        with patch.object(Path, "open", autospec=True, side_effect=open_log), \
                patch.object(provider.time, "monotonic", side_effect=AssertionError("no clock")), \
                patch.object(provider.time, "time", side_effect=AssertionError("no clock")), \
                patch.object(provider.time, "sleep", side_effect=AssertionError("no sleep")):
            gate.target_termination_diagnostics()
            gate.target_termination_diagnostics()
        self.assertEqual(polls, [0,1])
        self.assertEqual(paths, ["target-0-stderr.log","target-1-stderr.log"])
        self.assertEqual(sizes, [provider.TARGET_STDERR_LIMIT+1]*2)
        self.assertEqual({key:gate.result[key] for key in original}, original)
        result = gate.result["target_termination_diagnostics"]
        self.assertEqual(result["collection_phase"], "before_failure_cleanup")
        self.assertFalse(result["stream_eof_proved"])
        self.assertEqual(result["target_a"]["exit_class"], "running")
        self.assertEqual(result["target_b"]["exit_class"], "access_violation")
        self.assertTrue(result["target_a"]["report_write_error"])

    def test_absent_children_partial_truncated_and_failed_capture_remain_inconclusive(self):
        gate = Gate(FakeGui())
        with patch.object(Path, "open", side_effect=AssertionError("no unowned log read")) as opened:
            gate.target_termination_diagnostics()
        self.assertEqual(opened.call_count, 0)
        self.assertEqual(gate.result["target_termination_diagnostics"]["target_a"]["exit_class"], "unobserved")
        for raw, limit in ((b"Error: Write automation probe report",provider.TARGET_STDERR_LIMIT),
                (b"private secret path"*10,16)):
            gate = Gate(FakeGui())
            gate.targets_processes = [FakeGui(1)]
            with patch.object(provider, "TARGET_STDERR_LIMIT", limit), patch.object(Path, "open", return_value=io.BytesIO(raw)):
                gate.target_termination_diagnostics()
            row = gate.result["target_termination_diagnostics"]["target_a"]
            self.assertTrue(row["truncated"] or row["incomplete_line"])
            self.assertFalse(row["report_write_error"])
            self.assertNotIn("private secret path", repr(row))
        class BrokenTarget:
            def poll(self): raise RuntimeError("private secret path")
        gate = Gate(FakeGui())
        original = dict(gate.result)
        gate.targets_processes = [BrokenTarget(),FakeGui(0)]
        with patch.object(Path, "open", side_effect=OSError("private secret path")) as opened:
            gate.target_termination_diagnostics()
        self.assertEqual(opened.call_count, 2)
        self.assertEqual({key:gate.result[key] for key in original}, original)
        result = gate.result["target_termination_diagnostics"]
        self.assertEqual(result["target_a"]["poll_failure"], "UnexpectedError")
        self.assertEqual(result["target_b"]["exit_class"], "zero")
        self.assertIsNone(result["target_a"]["report_write_error"])
        self.assertNotIn("private secret path", repr(result))


class DirectReadinessTests(unittest.TestCase):
    def record(self, target=b"target_a", state=b"Prepared", error=b"None"):
        return (b"PIM_QUAL_READY_SHUTDOWN_V1 phase=after_existing_shutdown_attempt_before_worker_terminal target="+target+
            b" status=Observed present=Some(true) coherent=Some(true) header_matches=Some(true) "
            b"identity_matches=Some(true) build_matches=Some(true) binding_present=Some(true) state=Some("+state+
            b") error=Some("+error+b") identity_guard=IdentityValid\n")

    def test_direct_records_are_fixed_sanitized_facts_and_reject_near_misses(self):
        record = self.record()
        result = provider.direct_ready_lines(record+self.record(b"target_b", b"Poisoned", b"Unknown"))
        self.assertEqual(result["target_a"]["state"], "prepared")
        self.assertEqual(result["target_b"]["error"], "unknown")
        self.assertEqual(result["target_a_identity_guard"], "identity_valid")
        self.assertFalse(result["malformed_record"] or result["duplicate_role"])
        for bad in (record[:-1], b"private-prefix "+record,
                record.replace(b"present=Some(true)", b"present=1"),
                record.replace(b"identity_matches=Some(true)", b"identity_matches=Some(false)"),
                record.replace(b"error=Some(None)", b"error=Some(0xC0000001)"),
                record.replace(b"identity_guard=IdentityValid", b"identity_guard=ExitedBeforeRead"),
                record.replace(b"identity_guard=IdentityValid", b"identity_guard=private_pid_123"),
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

    def test_direct_identity_guard_classifies_only_fixed_unavailable_records(self):
        prefix = (b"PIM_QUAL_READY_SHUTDOWN_V1 phase="+provider.DIRECT_READY_PHASE+
            b" target=target_a status=IdentityUnavailable present=None coherent=None header_matches=None "
            b"identity_matches=None build_matches=None binding_present=None state=None error=None identity_guard=")
        guards = {b"IdentityMismatchOrInvalid":"identity_mismatch_or_invalid",b"ExitedBeforeRead":"exited_before_read",
            b"LivenessCheckFailedBeforeRead":"liveness_check_failed_before_read",b"ExitedAfterRead":"exited_after_read",
            b"LivenessCheckFailedAfterRead":"liveness_check_failed_after_read"}
        for value, expected in guards.items():
            result = provider.direct_ready_lines(prefix+value+b"\n")
            self.assertEqual(result["target_a_identity_guard"], expected)
            self.assertEqual(result["target_a"]["status"], "identity_unavailable")
            self.assertTrue(all(result["target_a"][key] is None for key in provider.DIRECT_READY_FIELDS[1:]))
            self.assertFalse(result["malformed_record"])
        for value in (b"IdentityValid", b"4294967295", b"private_pid_123"):
            result = provider.direct_ready_lines(prefix+value+b"\n")
            self.assertEqual(result["target_a_identity_guard"], "unobserved")
            self.assertEqual(result["target_a"]["status"], "unobserved")
            self.assertTrue(result["malformed_record"])
            self.assertNotIn("123", repr(result))
        invalid_payload = prefix.replace(b"present=None", b"present=Some(true)")+b"ExitedBeforeRead\n"
        self.assertTrue(provider.direct_ready_lines(invalid_payload)["malformed_record"])
        for missing in (b"", prefix+b"ExitedBeforeRead"):
            result = provider.direct_ready_lines(missing)
            self.assertEqual(result["target_a"]["status"], "unobserved")
            self.assertEqual(result["target_a_identity_guard"], "unobserved")

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
        for fail, diagnostic_throws in ((True,False),(True,True),(False,False)):
            with self.subTest(fail=fail, diagnostic_throws=diagnostic_throws), tempfile.TemporaryDirectory() as directory:
                trace, samples, target_samples = [], [], []

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

                    def target_termination_diagnostics(self):
                        trace.append("target_termination")
                        target_samples.append(dict(self.result))
                        if diagnostic_throws:
                            raise RuntimeError("private secret diagnostic failure")

                    def failed_group_cleanup(self):
                        trace.append("cleanup")
                        self.checkpoint, self.stage, self.check = 99, "cleanup", "cleanup"
                        self.result["failure_cleanup"] = {"outcome":"gui_exit_observed"}

                    def target_crash_event_diagnostics(self):
                        trace.append("crash_events")
                        assert self.result["failure_cleanup"]["outcome"] == "gui_exit_observed"
                        if diagnostic_throws:
                            raise RuntimeError("private secret event failure")

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
                self.assertEqual(trace, ["run", "before_failure_cleanup", "target_termination", "cleanup", "crash_events"] if fail else ["run", "after_run"])
                self.assertEqual(len(samples), 1)
                self.assertEqual(len(target_samples), 1 if fail else 0)
                result = json.loads(result_path.read_text(encoding="utf-8"))
                if fail:
                    frozen = {"status":"FAIL","checkpoint":11,"failure_stage":"prepare_holds",
                        "failure_predicate":"prepare_holds_target_held_state","failure_class":"Rejected",
                        "native_exit_state":"not_started"}
                    for key, value in frozen.items():
                        self.assertEqual(samples[0][key], value)
                        self.assertEqual(target_samples[0][key], value)
                        self.assertEqual(result[key], value)
                    self.assertNotIn("private secret path", repr(result))
                else:
                    self.assertEqual(samples[0]["status"], "PASS")
                    self.assertNotIn("failure_cleanup", result)


if __name__ == "__main__":
    unittest.main()
