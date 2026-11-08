#!/usr/bin/env python3
"""Headless portable smoke checks. Never starts routing, GUI, or input injection."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import time
import traceback


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def file_hashes(root):
    return {p.relative_to(root).as_posix(): sha256(p)
            for p in root.rglob("*") if p.is_file()
            and p.relative_to(root).parts[0].lower() != "data"
            and p.relative_to(root).as_posix() != ".data.config-lock"}


def execute(command, cwd, env, timeout=30):
    result = subprocess.run(command, cwd=cwd, env=env, capture_output=True,
                            text=True, encoding="utf-8", errors="replace", timeout=timeout)
    return {"exit_code": result.returncode, "stdout": result.stdout,
            "stderr": result.stderr}


class Mcp:
    def __init__(self, command, cwd, env):
        self.process = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        text=True, encoding="utf-8", errors="replace", bufsize=1)
        self.messages = queue.Queue()
        self.errors = []
        self.counter = 0
        self.methods = []
        threading.Thread(target=self._stdout, daemon=True).start()
        threading.Thread(target=self._stderr, daemon=True).start()

    def _stdout(self):
        try:
            for line in self.process.stdout:
                self.messages.put(json.loads(line))
        except Exception as error:
            self.messages.put(error)
        finally:
            self.messages.put(EOFError("MCP stdout closed"))

    def _stderr(self):
        for line in self.process.stderr:
            self.errors.append(line)

    def send(self, message):
        self.process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

    def request(self, method, params=None):
        self.counter += 1
        identity = self.counter
        self.methods.append(method)
        self.send({"jsonrpc": "2.0", "id": identity, "method": method,
                   "params": params or {}})
        deadline = time.monotonic() + 25
        while True:
            message = self.messages.get(timeout=max(0.01, deadline - time.monotonic()))
            if isinstance(message, Exception):
                raise RuntimeError(str(message) + "; stderr: " + "".join(self.errors))
            if message.get("id") == identity:
                require("error" not in message, f"MCP {method}: {message.get('error')}")
                return message["result"]
            require(time.monotonic() < deadline, f"MCP {method} response timeout")

    def initialize(self):
        result = self.request("initialize", {"protocolVersion": "2025-03-26",
                              "capabilities": {}, "clientInfo": {
                                  "name": "portable-headless-verification", "version": "1"}})
        require(result.get("serverInfo", {}).get("name"), "missing MCP server identity")
        require(result.get("capabilities", {}).get("tools") is not None,
                "missing MCP tools capability")
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        return result

    def call(self, name, arguments=None, expect_error=False):
        self.methods.append("tool:" + name)
        result = self.request("tools/call", {"name": name, "arguments": arguments or {}})
        require(bool(result.get("isError", False)) == expect_error,
                f"tool {name} unexpected error status: {result}")
        if expect_error:
            return result
        if "structuredContent" in result:
            return result["structuredContent"]
        text = "\n".join(c.get("text", "") for c in result.get("content", [])
                         if c.get("type") == "text")
        return json.loads(text)

    def close(self):
        if self.process.stdin and not self.process.stdin.closed:
            self.process.stdin.close()
        try:
            code = self.process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
            raise AssertionError("MCP failed to exit after stdin EOF")
        require(code == 0, f"MCP EOF exit {code}: {''.join(self.errors)}")

    def __enter__(self):
        return self

    def __exit__(self, kind, value, tb):
        if kind:
            if not self.process.stdin.closed:
                self.process.stdin.close()
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        else:
            self.close()


def verify_manifest(bundle):
    candidates = [bundle / "bundle-manifest.json", bundle / "manifest.json"]
    manifest_path = next((p for p in candidates if p.is_file()), None)
    require(manifest_path is not None, "bundle manifest is missing")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8-sig"))
    files = manifest["files"]
    require(isinstance(files, list) and files, "manifest files must be a nonempty array")
    listed = set()
    pe = []
    for entry in files:
        relative = Path(entry["path"])
        path = (bundle / relative).resolve()
        require(path.is_relative_to(bundle.resolve()), f"manifest path escapes bundle: {relative}")
        require(relative.as_posix().lower() not in listed, f"duplicate manifest path: {relative}")
        listed.add(relative.as_posix().lower())
        require(path.is_file(), f"packaged file missing: {relative}")
        require(sha256(path).lower() == entry["sha256"].lower(), f"hash mismatch: {relative}")
        expected_size = entry.get("bytes", entry.get("size"))
        if expected_size is not None:
            require(path.stat().st_size == expected_size, f"size mismatch: {relative}")
        if path.suffix.lower() in (".exe", ".dll"):
            with path.open("rb") as stream:
                require(stream.read(2) == b"MZ", f"invalid MZ: {relative}")
                stream.seek(0x3C)
                offset = struct.unpack("<I", stream.read(4))[0]
                stream.seek(offset)
                require(stream.read(4) == b"PE\0\0", f"invalid PE: {relative}")
                machine = struct.unpack("<H", stream.read(2))[0]
                require(machine in (0x14C, 0x8664), f"unexpected PE architecture: {relative}")
            if "32" in path.stem:
                require(machine == 0x14C, f"x86 runtime has wrong architecture: {relative}")
            if "64" in path.stem or path.name.lower() == "protoinputmultiplex.exe":
                require(machine == 0x8664, f"x64 binary has wrong architecture: {relative}")
            pe.append({"path": relative.as_posix(), "machine": hex(machine)})
    actual = {p.relative_to(bundle).as_posix().lower() for p in bundle.rglob("*")
              if p.is_file() and p != manifest_path}
    require(actual == listed, f"manifest coverage mismatch: missing={sorted(actual-listed)}, extra={sorted(listed-actual)}")
    require((bundle / "protoinputmultiplex.exe").is_file(), "application executable missing")
    for bits in (32, 64):
        require((bundle / "runtime" / f"protoinputmultiplex_cursor_observer{bits}.dll").is_file(),
                f"cursor observer missing: {bits}")
        for filename in (f"ProtoInputLoader{bits}.dll", f"ProtoInputHooks{bits}.dll",
                         f"ProtoInputIJ{bits}.exe", f"ProtoInputIJP{bits}.dll"):
            require((bundle / "runtime" / filename).is_file(), f"runtime missing: {filename}")
    return {"manifest": manifest_path.name, "files_verified": len(files), "pe_files": pe}


def run_checks(bundle, scratch, record):
    moved = scratch / "relocated portable bundle with spaces"
    shutil.copytree(bundle, moved)
    unrelated = scratch / "unrelated working directory"
    unrelated.mkdir()
    system_root = Path(os.environ.get("SystemRoot", r"C:\Windows"))
    env = os.environ.copy()
    env["PATH"] = str(system_root / "System32") + os.pathsep + str(system_root)
    env["LOCALAPPDATA"] = str(scratch / "isolated local app data")
    Path(env["LOCALAPPDATA"]).mkdir()
    env["TEMP"] = env["TMP"] = str(scratch)
    env.pop("RUST_LOG", None)
    exe = moved / "protoinputmultiplex.exe"
    record("manifest_hashes_and_pe", lambda: verify_manifest(moved))
    before = file_hashes(moved)

    def version(path):
        result = execute([str(path), "--version"], unrelated, env)
        require(result["exit_code"] == 0 and "protoinputmultiplex" in result["stdout"].lower(), str(result))
        return result
    record("relocated_version", lambda: version(exe))

    def devices(path):
        result = execute([str(path), "devices"], unrelated, env)
        require(result["exit_code"] == 0, str(result))
        inventory = json.loads(result["stdout"])
        require(isinstance(inventory, list), "devices output is not a JSON array")
        return {"exit_code": 0, "device_count": len(inventory),
                "scope": "read-only Raw Input inventory; no input emitted or captured"}
    record("relocated_devices", lambda: devices(exe))

    def doctor(runtime, expect_success, diagnostic):
        result = execute([str(exe), "doctor", "--runtime", str(runtime)], unrelated, env)
        require((result["exit_code"] == 0) == expect_success, str(result))
        require(diagnostic.lower() in (result["stdout"] + result["stderr"]).lower(), str(result))
        return result
    record("runtime_loader_exports", lambda: doctor(moved / "runtime", True, "all required API exports resolved"))

    def runtime_virtual_abi():
        # A separate interpreter bounds native load/version failures to this check.
        # Resolve the fence export, but never call it or any injection function.
        code = "\n".join([
            "import ctypes, json, sys",
            "assert ctypes.sizeof(ctypes.c_void_p) == 8, 'x64 Python required for x64 Loader ABI check'",
            "loader = ctypes.WinDLL(sys.argv[1], winmode=0x100 | 0x1000)",
            "version = loader.MuxInputVersion",
            "version.argtypes = []",
            "version.restype = ctypes.c_uint32",
            "actual = version()",
            "assert actual >= 3, f'virtual runtime ABI {actual} is older than required version 3'",
            "fence = loader.MuxQueueFence",
            "assert ctypes.cast(fence, ctypes.c_void_p).value, 'MuxQueueFence export address missing'",
            "print(json.dumps({'virtual_input_version': actual, 'required_minimum_version': 3, 'queue_fence_export_resolved': True, 'queue_fence_called': False, 'injection_attempted': False, 'restricted_load_flags': 'DLL_LOAD_DIR | DEFAULT_DIRS'}))",
        ])
        result = execute([str(Path(sys.executable).resolve()), "-I", "-c", code,
                          str(moved / "runtime" / "ProtoInputLoader64.dll")], unrelated, env)
        require(result["exit_code"] == 0, str(result))
        details = json.loads(result["stdout"])
        require(details["virtual_input_version"] >= 3 and details["queue_fence_export_resolved"],
                "bundled Loader virtual ABI check did not pass")
        return details
    record("runtime_virtual_abi_without_injection", runtime_virtual_abi)
    record("missing_runtime_folder_diagnostic", lambda: doctor(scratch / "absent runtime", False, "runtime folder missing"))
    missing_loader = scratch / "runtime without loader"
    shutil.copytree(moved / "runtime", missing_loader)
    (missing_loader / "ProtoInputLoader64.dll").unlink()
    record("missing_loader_diagnostic", lambda: doctor(missing_loader, False, "ProtoInputLoader64.dll"))

    dll_free = scratch / "DLL free application"
    dll_free.mkdir()
    free_exe = dll_free / exe.name
    shutil.copy2(exe, free_exe)
    # Keep only application-level CRT dependencies when the chosen build imports them.
    for dependency in moved.glob("*.dll"):
        shutil.copy2(dependency, dll_free / dependency.name)
    record("dll_free_devices", lambda: devices(free_exe))

    def mcp_readonly(command, preferences, local_env=env):
        with Mcp(command, unrelated, local_env) as client:
            initialized = client.initialize()
            tools = client.request("tools/list")["tools"]
            names = {tool["name"] for tool in tools}
            require({"routing_displays", "routing_state", "app_preferences"} <= names,
                    "required read-only tools missing")
            displays = client.call("routing_displays")
            require(isinstance(displays.get("monitors"), list) and "desktop" in displays,
                    "invalid display inventory")
            state = client.call("routing_state")
            require(state.get("running") is False, "unexpected running session")
            prefs = client.call("app_preferences", {"operation": "inspect"})
            require(Path(prefs["preferences_path"]).resolve() == preferences.resolve(),
                    f"preferences path mismatch: {prefs}")
            require(prefs["source"] == "standalone_document", "unexpected preference source")
            require(prefs["preferences"]["launch_at_login"] is False, "unexpected login preference")
            return {"server": initialized["serverInfo"], "protocol_version": initialized["protocolVersion"],
                    "tool_count": len(names), "monitor_count": len(displays["monitors"]),
                    "routing_running": state["running"], "preferences_path": str(preferences),
                    "methods": client.methods, "clean_eof_required": True}

    explicit = scratch / "explicit preferences" / "settings.json"
    direct = [str(free_exe), "mcp", "--runtime", str(dll_free / "absent runtime"),
              "--preferences", str(explicit)]
    record("dll_free_mcp_readonly_explicit_preferences", lambda: mcp_readonly(direct, explicit))

    no_local = env.copy()
    no_local.pop("LOCALAPPDATA", None)
    record("explicit_preferences_without_localappdata", lambda: mcp_readonly(direct, explicit, no_local))
    default_preferences = Path(env["LOCALAPPDATA"]) / "ProtoInputMultiPlex" / "app-preferences.json"
    record("default_localappdata_preferences", lambda: mcp_readonly(
        [str(free_exe), "mcp", "--runtime", str(dll_free / "absent runtime")], default_preferences))

    def missing_local():
        result = execute([str(free_exe), "mcp"], unrelated, no_local)
        require(result["exit_code"] != 0 and "LOCALAPPDATA is unavailable" in result["stderr"], str(result))
        return result
    record("missing_localappdata_diagnostic", missing_local)

    def configure_preferences():
        settings = {"launch_minimized": False, "minimize_to_tray": False,
                    "close_to_tray": False, "shortcuts": [],
                    "appearance_mode": "light", "zoom_percent": 105}
        with Mcp(direct, unrelated, env) as client:
            client.initialize()
            denied = client.call("app_preferences", {"operation": "configure", "settings": settings}, True)
            require("--allow-app-preferences" in json.dumps(denied), "missing explicit write-grant diagnostic")
        require(not explicit.exists(), "denied configuration wrote preferences")
        with Mcp(direct + ["--allow-app-preferences"], unrelated, env) as client:
            client.initialize()
            written = client.call("app_preferences", {"operation": "configure", "settings": settings})
            require(written["preferences"]["zoom_percent"] == 105, "configuration not acknowledged")
        persisted = json.loads(explicit.read_text(encoding="utf-8"))
        require(persisted["shortcuts"] == [] and persisted["launch_at_login"] is False,
                "unsafe or incorrect preference contents")
        with Mcp(direct, unrelated, env) as client:
            client.initialize()
            read = client.call("app_preferences", {"operation": "inspect"})
            require(read["preferences"] == persisted, "fresh process did not read persisted preferences")
        return {"path": str(explicit), "persisted": persisted,
                "write_denied_without_grant": True, "fresh_process_read_matches": True,
                "scope": "standalone document only; no shortcut or startup-registration APIs"}
    record("explicit_preferences_write_read", configure_preferences)

    def wrapper():
        script = moved / "launch-mcp.cmd"
        require(script.is_file(), "launch-mcp.cmd is missing")
        command = f'"{system_root / "System32" / "cmd.exe"}" /d /s /c ""{script}""'
        return mcp_readonly(command, moved / "data" / "app-preferences.json")
    record("mcp_cmd_wrapper_unrelated_cwd", wrapper)

    def wrapper_arguments():
        script = moved / "launch-mcp.cmd"
        command = f'"{system_root / "System32" / "cmd.exe"}" /d /s /c ""{script}" --help"'
        result = execute(command, unrelated, env)
        require(result["exit_code"] == 0 and "--gui-connection" in result["stdout"],
                f"MCP wrapper did not forward --help: {result}")
        return result
    record("mcp_cmd_wrapper_argument_forwarding", wrapper_arguments)

    def unchanged():
        after = file_hashes(moved)
        require(before == after, "packaged immutable files changed during verification")
        require(not list(unrelated.iterdir()), "launch created unexpected files in unrelated CWD")
        return {"immutable_files_checked": len(before), "data_directory_excluded": True,
                "unrelated_cwd_empty": True}
    record("packaged_files_unchanged", unchanged)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    report = {"schema_version": 1, "bundle": str(args.bundle.resolve()),
              "scope": "headless portable packaging verification", "checks": [],
              "gui_launch": "not attempted; requires coordinated native test slot",
              "routing_validation": "not attempted; no routing_start, virtual or physical input, or target injection",
              "runtime_backed_operation": "doctor resolves required Loader64 exports; isolated Python loads Loader64, calls MuxInputVersion and resolves MuxQueueFence without calling it; attachment/routing untested"}

    def record(name, action):
        started = time.monotonic()
        try:
            details = action()
            report["checks"].append({"name": name, "status": "PASS", "details": details,
                                      "seconds": round(time.monotonic() - started, 3)})
        except Exception as error:
            report["checks"].append({"name": name, "status": "FAIL", "error": str(error),
                                      "seconds": round(time.monotonic() - started, 3)})
        print(f"{report['checks'][-1]['status']}: {name}", flush=True)

    try:
        require(os.name == "nt", "portable launch verification requires Windows")
        with tempfile.TemporaryDirectory(prefix="ProtoInputMultiPlex portable clean ") as directory:
            scratch = Path(directory)
            report["disposable_directory"] = str(scratch)
            report["developer_path_removed"] = True
            run_checks(args.bundle.resolve(), scratch, record)
        report["disposable_directory_removed"] = True
    except Exception:
        report["fatal_error"] = traceback.format_exc()
    report["status"] = "PASS" if report["checks"] and all(
        check["status"] == "PASS" for check in report["checks"]) and "fatal_error" not in report else "FAIL"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"{report['status']}: report {args.output.resolve()}")
    return 0 if report["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
