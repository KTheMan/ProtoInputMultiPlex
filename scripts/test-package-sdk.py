"""Pure SDK staging rejection checks. Does not compile or launch product code."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("portable", Path(__file__).with_name("package-portable.py"))
portable = importlib.util.module_from_spec(spec)
spec.loader.exec_module(portable)


class SdkClosureTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.inputs = {"source_commit": "test-commit", "source_files": []}
        for architecture in portable.SDK_TARGETS:
            root = self.root/"sdk-release"/architecture
            root.mkdir(parents=True)
            names = portable.SDK_WEB_FILES if architecture == "web" else portable.SDK_NATIVE_FILES
            for name in names:
                item = root/name
                item.parent.mkdir(parents=True, exist_ok=True)
                item.write_bytes(name.encode())
            record = portable.sdk_identity(self.inputs, architecture)
            record["files"] = portable.sdk_records(root, names)
            (root/"metadata.json").write_text(json.dumps(record), encoding="utf-8")
        # These tests exercise metadata/hash closure, independent of PE/WASM parsing.
        self.content = patch.object(portable, "validate_sdk_content")
        self.content.start()
        self.addCleanup(self.content.stop)

    def test_complete_fixed_closure_keeps_both_same_named_dlls(self):
        inputs, records = portable.sdk_inputs(self.root, self.inputs)
        dlls = [relative.as_posix() for _, relative in inputs if relative.suffix == ".dll"]
        self.assertEqual(dlls, ["sdk/x64/mux_application_sdk.dll", "sdk/x86/mux_application_sdk.dll"])
        self.assertEqual(set(records), {"x64", "x86", "web"})

    def test_changed_payload_rejected(self):
        (self.root/"sdk-release/x86/mux_application_sdk.dll").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "digest mismatch"):
            portable.sdk_inputs(self.root, self.inputs)

    def test_mixed_architecture_metadata_rejected(self):
        root = self.root/"sdk-release"
        (root/"x86/metadata.json").write_bytes((root/"x64/metadata.json").read_bytes())
        with self.assertRaisesRegex(ValueError, "identity mismatch"):
            portable.sdk_inputs(self.root, self.inputs)

    def test_missing_and_extra_payloads_rejected(self):
        (self.root/"sdk-release/web/pkg/mux_application_web.d.ts").unlink()
        with self.assertRaisesRegex(ValueError, "Missing/extra"):
            portable.sdk_inputs(self.root, self.inputs)
        (self.root/"sdk-release/x64/native_dll_lifecycle.exe").write_bytes(b"fixture")
        with self.assertRaisesRegex(ValueError, "Missing/extra"):
            portable.sdk_inputs(self.root, self.inputs)

    def test_different_frozen_sources_rejected(self):
        with self.assertRaisesRegex(ValueError, "identity mismatch"):
            portable.sdk_inputs(self.root, {**self.inputs, "source_commit": "other"})


class SourceAdmissionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.committed = {"Cargo.toml", "scripts/package-portable.py"}
        self.candidates = set(self.committed)
        for name in self.committed:
            item = self.root/name
            item.parent.mkdir(parents=True, exist_ok=True)
            item.write_text("committed source", encoding="utf-8")
        self.patches = [patch.object(portable, "ROOT", self.root),
                        patch.object(portable, "REQUIRED_SOURCE_FILES", tuple(self.committed)),
                        patch.object(portable.subprocess, "check_output", side_effect=self.git_output)]
        for item in self.patches:
            item.start()
            self.addCleanup(item.stop)

    def git_output(self, args, cwd):
        self.assertEqual(cwd, self.root)
        names = self.committed if args[1] == "ls-tree" else self.candidates
        return ("\0".join(sorted(names))+"\0").encode()

    def private_file(self, name):
        item = self.root/name
        item.parent.mkdir(parents=True, exist_ok=True)
        item.write_text('{"event_credential":"PRIVATE-SENTINEL"}', encoding="utf-8")
        self.candidates.add(name)

    def test_private_untracked_candidates_fail_before_archive_admission(self):
        for name in ("scripts/private-acceptance/connection.json",
                     "crates/mux-application-web/private-evidence.json",
                     "packaging/private/session.json", "vendor/ProtoInput/private.json",
                     "examples/rust-mcp-client/src/private.rs"):
            with self.subTest(name=name):
                self.private_file(name)
                with self.assertRaisesRegex(ValueError, "Uncommitted source") as error:
                    portable.source_files()
                self.assertNotIn("PRIVATE-SENTINEL", str(error.exception))
                self.assertNotIn(name, str(error.exception))
                self.candidates.remove(name)

    def test_non_source_root_example_private_file_is_excluded(self):
        name = "examples/rust-mcp-client/src/connection.json"
        self.private_file(name)
        self.assertEqual(portable.source_files(), sorted(self.committed))

    def test_baseline_cannot_admit_private_file(self):
        name = "crates/mux-application-web/private-evidence.json"
        item = self.root/name
        item.parent.mkdir(parents=True)
        item.write_text("PRIVATE-SENTINEL", encoding="utf-8")
        # Simulate an ignored private file: absent even from Git's candidates.
        baseline = "source-baseline.json"
        (self.root/baseline).write_text(json.dumps({"files": {name: "old-hash"}}), encoding="utf-8")
        self.committed.add(baseline)
        self.candidates.add(baseline)
        self.assertNotIn(name, portable.source_files())

    def test_required_source_not_committed_is_rejected(self):
        with patch.object(portable, "REQUIRED_SOURCE_FILES", ("scripts/new-builder.py",)):
            with self.assertRaisesRegex(ValueError, "not committed"):
                portable.source_files()

    def test_tracked_compatibility_patch_current_bytes_are_retained(self):
        name = "vendor/ProtoInput/patched.cpp"
        item = self.root/name
        item.parent.mkdir(parents=True)
        item.write_text("current compatibility patch", encoding="utf-8")
        self.committed.add(name)
        self.candidates.add(name)
        self.assertIn(name, portable.source_files())
        self.assertEqual((self.root/name).read_text(), "current compatibility patch")

    def prepared_patch_fixture(self, extra_private=False, tampered_header=False, changed_patch=False, outside=False):
        # Mock Git's patch engine, but require the complete production order and
        # final blobs after all stages. The real engine is exercised by release
        # cloud validation; no Git/native commands execute in these pure tests.
        patches = {}
        for number, patch_name in enumerate(portable.BACKEND_PATCH_SEQUENCE):
            data = (f"committed patch stage {number}\n").encode()
            patch_path = self.root/patch_name
            patch_path.parent.mkdir(parents=True, exist_ok=True)
            patch_path.write_bytes(data + (b"changed" if changed_patch and not number else b""))
            patches[patch_name] = data
            self.committed.add(patch_name)
            self.candidates.add(patch_name)
        relatives = ["src/ProtoInput/ProtoInputHooks/"+name for name in (
            "MuxCursorObserverBridge.h", "MuxLifecycleNative.h", "pipeinclude/MuxLifecycleProtocol.h",
            "MuxKeyboardStateBridge.h", "pipeinclude/MuxSetupAckProtocol.h")]
        vendor = self.root/"vendor/ProtoInput"
        (vendor/".git").mkdir(parents=True)
        blobs = {}
        for relative in relatives:
            header = vendor/relative
            header.parent.mkdir(parents=True, exist_ok=True)
            # The final cursor/lifecycle bytes represent later overlay edits,
            # not the initial new-file addition. Every blob is read after replay.
            blobs[relative] = ("final ordered closure: "+relative+"\n").encode()
            header.write_bytes(b"private changed content" if tampered_header else blobs[relative].replace(b"\n", b"\r\n"))
        applied = []
        replay_environment = []
        def output(args, cwd, env=None):
            if cwd == self.root and args[1] == "show":
                self.assertIsNone(env)
                return patches[args[2].removeprefix("HEAD:")]
            if cwd == vendor:
                if args[1] == "rev-parse":
                    self.assertIsNone(env)
                    return (portable.PIN if args[2] == "HEAD" else str(vendor/".git/objects")).encode()
                if args[1:3] == ["-c", "core.autocrlf=false"]:
                    self.assertEqual(args[3:7], ["-c", "core.splitIndex=false", "-c", "core.fsmonitor=false"])
                    args = args[:3] + args[7:]
                    operation = args[3]
                    self.assertIsNotNone(env)
                    self.assertNotEqual(Path(env["GIT_INDEX_FILE"]), vendor/".git/index")
                    self.assertFalse(Path(env["GIT_OBJECT_DIRECTORY"]).is_relative_to(vendor))
                    self.assertEqual(json.loads(env["GIT_ALTERNATE_OBJECT_DIRECTORIES"]), str(vendor/".git/objects"))
                    replay_environment.append(env)
                    if operation == "read-tree":
                        self.assertEqual(args[4], portable.PIN)
                        self.assertFalse(Path(env["GIT_INDEX_FILE"]).exists())
                        return b""
                    if operation == "apply":
                        number = len(applied)
                        self.assertEqual(args[4:-1], ["--cached", "--ignore-space-change"] + (["--whitespace=nowarn"] if number else []))
                        self.assertEqual(Path(args[-1]).read_bytes(), patches[portable.BACKEND_PATCH_SEQUENCE[number]])
                        applied.append(portable.BACKEND_PATCH_SEQUENCE[number])
                        return b""
                    self.assertEqual(applied, list(portable.BACKEND_PATCH_SEQUENCE))
                    if operation == "diff":
                        self.assertEqual(args[4:], ["--cached", "--no-renames", "--diff-filter=A", "--name-only", "-z", portable.PIN])
                        return (("../outside.h" if outside else "\0".join(relatives))+"\0").encode()
                    self.assertEqual(operation, "show")
                    return blobs[args[4].removeprefix(":")]
                if args[1] == "ls-tree":
                    return b""
                self.assertIsNone(env)
                names = relatives + (["private-acceptance/connection.json"] if extra_private else [])
                return ("\0".join(names)+"\0").encode()
            return self.git_output(args, cwd)
        return relatives, output, applied

    def test_exact_committed_patch_created_header_admitted_from_submodule(self):
        relatives, output, applied = self.prepared_patch_fixture()
        with patch.object(portable.subprocess, "check_output", side_effect=output):
            names = portable.source_files()
            self.assertTrue(all("vendor/ProtoInput/"+relative in names for relative in relatives))
            self.assertEqual(applied, list(portable.BACKEND_PATCH_SEQUENCE))

    def test_patch_created_header_does_not_admit_extra_private_file(self):
        _, output, _ = self.prepared_patch_fixture(extra_private=True)
        with patch.object(portable.subprocess, "check_output", side_effect=output):
            with self.assertRaisesRegex(ValueError, "Uncommitted source"):
                portable.source_files()

    def test_patch_created_header_bytes_must_match_committed_patch(self):
        _, output, _ = self.prepared_patch_fixture(tampered_header=True)
        with patch.object(portable.subprocess, "check_output", side_effect=output):
            with self.assertRaisesRegex(ValueError, "header differs"):
                portable.source_files()

    def test_modified_patch_cannot_authorize_private_header_content(self):
        _, output, _ = self.prepared_patch_fixture(changed_patch=True)
        with patch.object(portable.subprocess, "check_output", side_effect=output):
            with self.assertRaisesRegex(ValueError, "patch differs"):
                portable.source_files()

    def test_outside_patch_added_path_rejected(self):
        _, output, _ = self.prepared_patch_fixture(outside=True)
        with patch.object(portable.subprocess, "check_output", side_effect=output):
            with self.assertRaisesRegex(ValueError, "outside approved"):
                portable.source_files()


if __name__ == "__main__":
    unittest.main()
