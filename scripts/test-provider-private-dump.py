"""Pure offline boundary fixtures; no Windows tool, target or debugger starts."""
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("private_dump", Path(__file__).with_name("provider-private-dump.py"))
dump = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dump)


def records(identity=(42, 133000000000000000), rip=0xDEAD, rsp=0x1000):
    setup = [dump.FATAL_MAGIC, 1, 1152, *identity, 1, 0]+[0]*8+[dump.FATAL_MAGIC]
    fatal = setup[:5]+[2, 0, 0, 0xC0000005, 0, rip, 0, 0, 0, 0, dump.FATAL_MAGIC]
    raw = struct.pack("<16Q", *setup)+bytes(1024)+struct.pack("<16Q", *fatal)+bytes(1024)
    status = {"schema":"pim.private-owned-minidump.v1", "status":"complete", "failure_reason":None, "pid":identity[0],
        "creation":identity[1], "thread_id":44, "exception_code":0xC0000005, "filter_result":0,
        "original_rip":rip, "original_rsp":rsp, "original_context_flags":0x100003,
        "basic_context_only":True, "thread_owner_pid_verified":True, "dump_bytes":4096,
        "one_attempt":True, "deadline_tick_ms":9999, "source_commit":"a"*40,
        "thread_creation":133000000000000001, "exception_address":rip, "exception_flags":0,
        "exception_parameter_count":2, "exception_parameters":[0]*15,
        "integer_registers":[0]*4+[rsp]+[0]*11+[rip]}
    wait = struct.pack("<8Q", dump.WAIT_MAGIC, 1, *identity, 44, 0, 4, 9999)
    return raw, status, wait


def minidump(status):
    raw = bytearray(4096)
    raw[:4] = b"MDMP"
    struct.pack_into("<IIIIIQ", raw, 4, 0xA793, 5, 32, 0, 0, 0x7820)
    for index, (kind, size, offset) in enumerate(((3, 52, 128), (4, 112, 256), (6, 168, 512), (7, 56, 768), (17, 76, 848))):
        struct.pack_into("<III", raw, 32+12*index, kind, size, offset)
    struct.pack_into("<I", raw, 128, 1)
    struct.pack_into("<I", raw, 132, 44)
    struct.pack_into("<QII", raw, 132+24, 0x1000, 64, 2048)
    struct.pack_into("<II", raw, 132+40, 256, 1024)
    struct.pack_into("<I", raw, 256, 1)
    struct.pack_into("<QIIII", raw, 260, 0x2000, 0x1000, 0, 1234, 3000)
    name = r"C:\private\protoinputmultiplex.exe".encode("utf-16-le")
    struct.pack_into("<I", raw, 3000, len(name)); raw[3004:3004+len(name)] = name
    cv = b"RSDS"+bytes(range(16))+struct.pack("<I", 1)+b"local.pdb\0"
    struct.pack_into("<II", raw, 260+76, len(cv), 3200); raw[3200:3200+len(cv)] = cv
    struct.pack_into("<II", raw, 512, 44, 0)
    struct.pack_into("<IIQQII", raw, 520, 0xC0000005, 0, 0, status["original_rip"], 2, 0)
    struct.pack_into("<II", raw, 512+160, 256, 1024)
    struct.pack_into("<H", raw, 768, 9)
    struct.pack_into("<III", raw, 848, 12, 64, 1)
    struct.pack_into("<I", raw, 860, 44)
    struct.pack_into("<Q", raw, 876, status["thread_creation"])
    struct.pack_into("<I", raw, 1024+48, 0x100003)
    struct.pack_into("<Q", raw, 1024+152, status["original_rsp"])
    struct.pack_into("<Q", raw, 1024+248, status["original_rip"])
    struct.pack_into("<Q", raw, 2048, 0x2505)
    return bytes(raw)


def pdb(guid=bytes(range(16)), age=1):
    raw = bytearray(512*5)
    raw[:32] = b"Microsoft C/C++ MSF 7.00\r\n\x1aDS\0\0\0"
    struct.pack_into("<6I", raw, 32, 512, 1, 5, 16, 0, 2)
    struct.pack_into("<I", raw, 1024, 3)
    struct.pack_into("<4I", raw, 1536, 2, 0, 28, 4)
    struct.pack_into("<III", raw, 2048, 20000404, 0, age)
    raw[2060:2076] = guid
    return bytes(raw)


class OwnedIdentityTests(unittest.TestCase):
    def validate(self, fatal, status, wait, identity=(42, 133000000000000000), exit_class="access_violation"):
        return dump.owned_records(fatal, json.dumps(status).encode(), wait, identity, exit_class, 9999, "a"*40)

    def test_complete_identity_and_nonimage_pc_admitted_without_image_assumption(self):
        fatal, status, wait = records()
        self.assertEqual(self.validate(fatal, status, wait), status)
        snapshot = dump.dump(minidump(status), status)
        self.assertEqual(snapshot["rip"], 0xDEAD)
        self.assertEqual(snapshot["candidates"], [0x2505])

    def test_partial_mismatched_recovered_and_timed_out_never_admitted(self):
        fatal, status, wait = records()
        for bad_fatal, bad_status, bad_wait, identity, exit_class in (
            (fatal[:1152], status, wait, (42,133000000000000000), "access_violation"),
            (fatal, status, wait, (43,133000000000000000), "access_violation"),
            (fatal, status, wait, (42,133000000000000001), "access_violation"),
            (fatal, status, wait, (42,133000000000000000), "running"),
            (fatal, dict(status, status="canceled"), wait, (42,133000000000000000), "access_violation"),
            (fatal, dict(status, thread_id=45), wait, (42,133000000000000000), "access_violation"),
            (fatal, dict(status, original_rip=0xBAD), wait, (42,133000000000000000), "access_violation"),
            (fatal, dict(status, one_attempt=1), wait, (42,133000000000000000), "access_violation"),
            (fatal, status, wait[:-1], (42,133000000000000000), "access_violation"),
            (fatal, status, struct.pack("<8Q",dump.WAIT_MAGIC,1,42,133000000000000000,44,258,4,9999), (42,133000000000000000), "access_violation"),
        ):
            with self.subTest(status=bad_status), self.assertRaises(ValueError):
                self.validate(bad_fatal, bad_status, bad_wait, identity, exit_class)

    def test_context_and_directory_bounds_reject_before_any_tool(self):
        _, status, _ = records()
        good = minidump(status)
        for offset, fmt, value in ((40,"<I",4090), (512+160,"<I",100000),
                (1024+248,"<Q",0xBAD), (1024+152,"<Q",0xBAD),
                (1024+48,"<I",0x100043), (768,"<H",0), (24,"<Q",2),
                (260+76,"<I",999999), (3000,"<I",0xFFFFFFFF)):
            raw = bytearray(good); struct.pack_into(fmt,raw,offset,value)
            with self.subTest(offset=offset), self.assertRaises(ValueError):
                dump.dump(bytes(raw),status)
        with self.assertRaises(ValueError): dump.dump(good[:100],status)


class SymbolIdentityTests(unittest.TestCase):
    def test_msf_guid_age_and_bad_block_directory(self):
        self.assertEqual(dump.pdb_identity(pdb()), (bytes(range(16)),1))
        self.assertNotEqual(dump.pdb_identity(pdb(age=2)), (bytes(range(16)),1))
        raw = bytearray(pdb()); struct.pack_into("<I", raw, 1024, 9999)
        with self.assertRaises(ValueError): dump.pdb_identity(bytes(raw))
        with self.assertRaises(ValueError): dump.pdb_identity(pdb()[:-1])

    def test_disk_pair_is_insufficient_without_dump_cv_identity(self):
        row = {"path":r"C:\private\protoinputmultiplex.exe", "cv":(b"X"*16,1), "size":8192,"stamp":123}
        item = {"image":row["path"],"pdb":r"C:\private\protoinputmultiplex.pdb", "image_sha256":"a"*64,"pdb_sha256":"b"*64}
        with patch.object(dump,"digest",side_effect=["a"*64,"b"*64]), \
                patch.object(dump,"bounded",return_value=b"fixture"), \
                patch.object(dump,"pe",return_value={"cv":(bytes(range(16)),1),"size":8192,"stamp":123,"pdb_path":item["pdb"]}), \
                patch.object(dump,"pdb_identity",return_value=(bytes(range(16)),1)):
            with self.assertRaises(ValueError): dump.admitted_module(row,[item])

    def test_matching_dump_pe_pdb_guid_age_size_stamp_and_paths_all_required(self):
        cv = (bytes(range(16)),1)
        row = {"path":r"C:\private\protoinputmultiplex.exe", "cv":cv, "size":8192,"stamp":123}
        item = {"image":row["path"],"pdb":r"C:\private\protoinputmultiplex.pdb", "image_sha256":"a"*64,"pdb_sha256":"b"*64}
        image = {"cv":cv,"size":8192,"stamp":123,"pdb_path":item["pdb"]}
        for mutation in ({}, {"cv":(cv[0],2)}, {"size":8193}, {"stamp":124},
                {"pdb_path":r"C:\other\secret.pdb"}, {"pdb_path":"other.pdb"}):
            with patch.object(dump,"digest",side_effect=["a"*64,"b"*64]), \
                    patch.object(dump,"bounded",return_value=b"fixture"), \
                    patch.object(dump,"pe",return_value=dict(image,**mutation)), \
                    patch.object(dump,"pdb_identity",return_value=cv):
                if mutation:
                    with self.assertRaises(ValueError): dump.admitted_module(row,[item])
                else:
                    self.assertEqual(dump.admitted_module(row,[item]),(item,image))

    def test_relative_codeview_basename_matches_exact_admitted_pdb(self):
        cv=(bytes(range(16)),1)
        row={"path":r"C:\private\protoinputmultiplex.exe","cv":cv,"size":8192,"stamp":123}
        item={"image":row["path"],"pdb":r"C:\private\protoinputmultiplex.pdb",
            "image_sha256":"a"*64,"pdb_sha256":"b"*64}
        image={"cv":cv,"size":8192,"stamp":123,"pdb_path":"protoinputmultiplex.pdb"}
        with patch.object(dump,"digest",side_effect=["a"*64,"b"*64]), \
                patch.object(dump,"bounded",return_value=b"fixture"), \
                patch.object(dump,"pe",return_value=image), \
                patch.object(dump,"pdb_identity",return_value=cv):
            self.assertEqual(dump.admitted_module(row,[item]),(item,image))

    def test_relative_codeview_path_must_be_bare_and_pdb_adjacent(self):
        image=r"C:\private\protoinputmultiplex.exe"
        pdb_path=r"C:\private\protoinputmultiplex.pdb"
        self.assertTrue(dump.pdb_path_matches("protoinputmultiplex.pdb",image,pdb_path))
        for codeview_path in (r"..\private\protoinputmultiplex.pdb",
                r"symbols\protoinputmultiplex.pdb",r"C:protoinputmultiplex.pdb"):
            self.assertFalse(dump.pdb_path_matches(codeview_path,image,pdb_path))
        self.assertFalse(dump.pdb_path_matches("protoinputmultiplex.pdb",image,
            r"C:\symbols\protoinputmultiplex.pdb"))

    def test_source_requires_exact_allowlisted_same_source_hash(self):
        raw = json.dumps([{"Symbol":[{"FileName":r"C:\source\crates\probe.rs", "FunctionName":"probe::run", "Line":12}]}]).encode()
        allowed = {"crates/probe.rs":"a"*64}
        with patch.object(dump,"digest",return_value="a"*64):
            self.assertEqual(dump.source_location(raw,r"C:\source",allowed),
                {"file":"crates/probe.rs","function":"probe::run","line":12})
            with self.assertRaises(ValueError): dump.source_location(raw,r"C:\source",{})
            with self.assertRaises(ValueError): dump.source_location(raw.replace(b"probe::run",b"private\\\\secret"),r"C:\source",allowed)
        with patch.object(dump,"digest",return_value="b"*64):
            with self.assertRaises(ValueError): dump.source_location(raw,r"C:\source",allowed)


class CallerEvidenceTests(unittest.TestCase):
    def transcript(self, warning="", instruction="00000000`00002500 e800000000 call 00000000`00003000"):
        return ("PIM_STACK_BEGIN\n"+warning+"\n00 00000000`00001000 00000000`00002505 unknown\n"
            "01 00000000`00001008 00000000`00000000 own!caller\nPIM_STACK_END\n"
            "PIM_FRAME_01_BEGIN\nPIM_REG 00000000`00002505 00000000`00001008\n"+instruction+
            "\nPIM_FRAME_01_END\n").encode()

    def test_nonimage_pc_with_callsite_remains_a_caller_candidate(self):
        snapshot={"rip":0xDEAD,"rsp":0x1000,"candidates":[0x2505]}
        self.assertEqual(dump.parse_cdb(self.transcript(),snapshot),[(0x2505,"callsite_candidate")])
        warning="WARNING: Stack unwind information not available. Following frames may be wrong."
        self.assertEqual(dump.parse_cdb(self.transcript(warning),snapshot),[(0x2505,"uncertain_callsite_candidate")])

    def test_raw_stack_word_or_noncall_is_never_a_proven_caller(self):
        snapshot={"rip":0xDEAD,"rsp":0x1000,"candidates":[0x2505]}
        self.assertEqual(dump.parse_cdb(self.transcript(instruction="00000000`00002504 90 nop"),snapshot),[])
        self.assertEqual(dump.parse_cdb(self.transcript().replace(b"PIM_REG 00000000`00002505",b"PIM_REG 00000000`00002506"),snapshot),[])


class PreservationTests(unittest.TestCase):
    def test_frozen_fail_and_cleanup_preserved_and_no_new_process_poll(self):
        gate = SimpleNamespace(args=SimpleNamespace(private_dump_decision=None),private_dump_requested=True,
            result={"status":"FAIL","checkpoint":11,"failure_stage":"prepare_holds", "failure_predicate":"held_state",
                "failure_class":"Rejected","failure_cleanup":{"outcome":"gui_exit_observed"}})
        before=json.loads(json.dumps(gate.result))
        dump.finish(gate)
        self.assertEqual({key:gate.result[key] for key in before},before)
        self.assertFalse(gate.result["functional_acceptance"])
        public=json.dumps(gate.result["private_dump_diagnostics"])
        for forbidden in ("pid", "original_rip", "original_rsp", "C:\\", "register", "stdout"):
            self.assertNotIn(forbidden,public)

    def test_capture_enabled_success_never_claims_functional_pass(self):
        gate=SimpleNamespace(args=SimpleNamespace(private_dump_decision=None),private_dump_requested=True,result={"status":"PASS"})
        dump.finish(gate)
        self.assertEqual(gate.result["status"],"DIAGNOSTIC_ONLY")
        self.assertFalse(gate.result["functional_acceptance"])

    def test_prepare_failure_phase_is_fixed_and_private(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)/"private-output"
            gate=SimpleNamespace(args=SimpleNamespace(private_dump_root=root,
                private_dump_metadata=Path(directory)/"metadata.private.json",commit="a"*40,
                source=Path(directory),private_dump_decision=None),end=100,
                result={"status":"FAIL","checkpoint":11})
            with patch.object(dump,"bounded",side_effect=ValueError("private path and content")):
                with self.assertRaises(ValueError): dump.prepare(gate)
            dump.finish(gate)
        decision=gate.result["private_dump_diagnostics"]
        self.assertEqual(decision["preparation"],{"status":"not_ready","phase":"metadata"})
        public=json.dumps(decision)
        self.assertNotIn(directory,public)
        self.assertNotIn("private path and content",public)
        self.assertEqual(gate.result["status"],"FAIL")

    def test_invalid_prepare_phase_is_normalized_to_fixed_enum(self):
        gate=SimpleNamespace(args=SimpleNamespace(private_dump_decision=None),private_dump_requested=True,
            private_dump_prepare_phase="private-content",result={"status":"FAIL"})
        dump.finish(gate)
        self.assertEqual(gate.result["private_dump_diagnostics"]["preparation"],
            {"status":"not_ready","phase":"not_started"})

    def test_probe_identity_rejection_has_bounded_subphase(self):
        cases=("probe_file_hashes", "probe_pe_codeview", "probe_pdb_identity", "probe_identity_match")
        for phase in cases:
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as directory:
                root=Path(directory)/"private-output"
                source=Path(directory)
                probe={"image":r"C:\build\protoinputmultiplex.exe","image_sha256":"a"*64,
                    "pdb":r"C:\build\protoinputmultiplex.pdb","pdb_sha256":"b"*64}
                metadata={"schema":"pim.private-dump-build.v1","source_commit":"a"*40,
                    "profile":"debug","dev_debug":2,"features":["software-renderer"],
                    "target":"x86_64-pc-windows-msvc","source":str(source.resolve()),
                    "source_files":{},"probe":probe}
                gate=SimpleNamespace(args=SimpleNamespace(private_dump_root=root,
                    private_dump_metadata=Path(directory)/"metadata.private.json",commit="a"*40,
                    source=source,private_dump_decision=None),end=100,result={"status":"FAIL"})
                digest_patch=patch.object(dump,"digest",side_effect=ValueError) if phase == "probe_file_hashes" else \
                    patch.object(dump,"digest",side_effect=["a"*64,"b"*64])
                pe_patch=patch.object(dump,"pe",side_effect=ValueError) if phase == "probe_pe_codeview" else \
                    patch.object(dump,"pe",return_value={"cv":(bytes(range(16)),1),"pdb_path":probe["pdb"]})
                pdb_patch=patch.object(dump,"pdb_identity",side_effect=ValueError) if phase == "probe_pdb_identity" else \
                    patch.object(dump,"pdb_identity",return_value=(b"Z"*16,1))
                with patch.object(dump,"strict_json",return_value=metadata), \
                        patch.object(dump,"bounded",return_value=b"fixture"), digest_patch, pe_patch, pdb_patch:
                    with self.assertRaises(ValueError): dump.prepare(gate)
                dump.finish(gate)
                self.assertEqual(gate.result["private_dump_diagnostics"]["preparation"],
                    {"status":"not_ready","phase":phase})

    def test_ineligible_and_expired_inputs_start_no_offline_analysis(self):
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(dump,"analyze",side_effect=AssertionError("no analysis")) as analyze, \
                patch.object(dump,"bounded",side_effect=AssertionError("no reads")) as read, \
                patch.object(dump.time,"monotonic",return_value=10):
            output=dump.collect(Path(directory),{}, {},{},11)
            self.assertEqual(output["target_a"]["reason"],"ineligible")
            output=dump.collect(Path(directory),{}, {"target_a":(42,1)},{"target_a":"access_violation"},9)
            self.assertEqual(output["target_a"]["reason"],"deadline_exhausted")
            analyze.assert_not_called(); read.assert_not_called()

    def test_collection_reports_fixed_rejection_boundaries(self):
        identity = (42, 133000000000000000)
        fatal, status, wait = records(identity)
        metadata = {"deadline_tick_ms": 9999, "source_commit": "a" * 40}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, content, reason in (
                ("fatal.private.bin", fatal, "capture_records_unavailable"),
                ("dump-wait.private.bin", wait[:-1], "capture_records_unavailable"),
                ("dump-status.private.json", json.dumps(status).encode(), "capture_identity_unconfirmed"),
                ("dump-wait.private.bin", wait, "dump_bytes_unconfirmed"),
                ("dump.private.dmp", b"private payload", "dump_bytes_unconfirmed"),
                ("dump.private.dmp", bytes(4096), "dump_structure_unconfirmed"),
            ):
                with self.subTest(reason=reason, name=name), \
                        patch.object(dump, "analyze", side_effect=AssertionError("no analysis")):
                    (root / f"target-0.{name}").write_bytes(content)
                    result = dump.collect(root, metadata, {"target_a": identity},
                                          {"target_a": "access_violation"}, float("inf"))
                    self.assertEqual(result["target_a"], dump.empty(reason))

    def test_collection_distinguishes_capture_completion_failures(self):
        identity = (42, 133000000000000000)
        fatal, status, wait = records(identity)
        metadata = {"deadline_tick_ms": 9999, "source_commit": "a" * 40}
        encoded_status = json.dumps(status).encode()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "target-0.fatal.private.bin").write_bytes(fatal)
            for status_raw, wait_raw, reason in (
                (b"", wait, "capture_status_empty"),
                (encoded_status, b"", "capture_wait_empty"),
                (json.dumps(dict(status, status="failed", failure_reason="dump_partial_copy", dump_bytes=0)).encode(), wait,
                 "capture_writer_dump_partial_copy"),
                (json.dumps(dict(status, status="failed", failure_reason="callback_write", pid=43)).encode(), wait,
                 "capture_identity_unconfirmed"),
                (json.dumps(dict(status, status="failed", failure_reason="raw unexpected text")).encode(), wait,
                 "capture_identity_unconfirmed"),
                (json.dumps(dict(status, failure_reason="sync")).encode(), wait,
                 "capture_identity_unconfirmed"),
                (encoded_status, struct.pack("<8Q", dump.WAIT_MAGIC, 1, *identity, 44, 258, 4, 9999),
                 "capture_wait_timeout"),
                (encoded_status, struct.pack("<8Q", dump.WAIT_MAGIC, 1, *identity, 44, 0, 5, 9999),
                 "capture_wait_not_complete"),
                (encoded_status, struct.pack("<8Q", dump.WAIT_MAGIC, 1, *identity, 45, 258, 4, 9999),
                 "capture_identity_unconfirmed"),
            ):
                with self.subTest(reason=reason), \
                        patch.object(dump, "analyze", side_effect=AssertionError("no analysis")):
                    (root / "target-0.dump-status.private.json").write_bytes(status_raw)
                    (root / "target-0.dump-wait.private.bin").write_bytes(wait_raw)
                    result = dump.collect(root, metadata, {"target_a": identity},
                                          {"target_a": "access_violation"}, float("inf"))
                    self.assertEqual(result["target_a"], dump.empty(reason))

    def test_symbol_source_proxy_environment_never_inherited(self):
        with patch.dict(dump.os.environ,{"_NT_SYMBOL_PATH":"srv*https://private", "HTTPS_PROXY":"private", "TOKEN":"private"}):
            environment=dump.clean_environment(Path("C:/private"))
        self.assertEqual(environment["_NT_SYMBOL_PATH"],"")
        self.assertNotIn("HTTPS_PROXY",environment)
        self.assertNotIn("TOKEN",environment)
        self.assertNotIn("srv*",repr(environment))


if __name__ == "__main__":
    unittest.main()
