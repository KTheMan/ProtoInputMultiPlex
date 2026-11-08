"""Independent finite app-preference/lifecycle acceptance.

Preparation must use AST inspection only. --ready is required before importing
native helpers or launching any executable. GUI/native phases are added against
the frozen source contract; this document phase does not certify tray behavior.
"""
import argparse
import copy
from contextlib import contextmanager
import hashlib
import importlib.util
import json
from pathlib import Path
import queue
import shutil
import subprocess
import threading
import time

def load_module(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result

def put(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")

def digest(path): return hashlib.sha256(path.read_bytes()).hexdigest()

def capture_harness(run):
    scripts=Path(__file__).resolve().parent
    target=run/"runner-source";target.mkdir(exist_ok=True)
    for name in("test-app-lifecycle-acceptance.py","test-app-lifecycle-ui.py","owned-app-hotkeys.py","owned-tray-menu.py"):
        shutil.copy2(scripts/name,target/name)
    put(target/"dependency-hashes.json",{p.name:digest(p)for p in scripts.glob("*.py")})

@contextmanager
def deny_owned_file_replacement(path):
    """Keep the actual owned destination readable but deny rename/delete."""
    import ctypes
    from ctypes import wintypes
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.CreateFileW.argtypes = [wintypes.LPCWSTR,wintypes.DWORD,wintypes.DWORD,
        ctypes.c_void_p,wintypes.DWORD,wintypes.DWORD,wintypes.HANDLE]
    kernel.CreateFileW.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = kernel.CreateFileW(str(path),0x80000000,1,None,3,0,None)
    assert handle not in (None,ctypes.c_void_p(-1).value),ctypes.get_last_error()
    try: yield
    finally: assert kernel.CloseHandle(handle)

def client_type(base):
    class LifecycleClient(base.Client):
        def __init__(self, executable, preferences, runtime, log, allow=False):
            self.stderr = log.open("w", encoding="utf-8")
            command = [str(executable), "mcp", "--runtime", str(runtime), "--preferences", str(preferences)]
            if allow: command.append("--allow-app-preferences")
            self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=self.stderr, text=True, encoding="utf-8", creationflags=subprocess.CREATE_NO_WINDOW)
            self.messages, self.next_id, self.latest = queue.Queue(), 0, False
            threading.Thread(target=self.read, daemon=True).start()
            try:
                initialized = self.rpc("initialize", {"protocolVersion":"2025-11-25", "capabilities":{},
                    "clientInfo":{"name":"owned-app-lifecycle-acceptance", "version":"1"}})
                assert initialized["capabilities"]["tools"] is not None
                self.send({"jsonrpc":"2.0", "method":"notifications/initialized"})
            except Exception:
                self.process.kill(); self.process.wait(timeout=10); self.stderr.close()
                raise
    return LifecycleClient

def document_group(args):
    """One document/provider group; no DLLs, worker, registry or native targets."""
    base = load_module("lifecycle_mcp", "test-mcp.py")
    Client = client_type(base)
    run = args.runtime.resolve()/time.strftime("acceptance-app-lifecycle-documents-%Y%m%d-%H%M%S")
    run.mkdir(parents=True)
    capture_harness(run)
    image = run/"owned control64.exe"; shutil.copy2(args.executable, image)
    prefs = run/"owned preferences.json"
    missing_runtime = run/"no-runtime"; missing_runtime.mkdir()
    profile = run/"untouched-profile.json"
    # Nontrivial complete owned document; no live PID assertion or attachment.
    put(profile, {"version":1,"concurrent":True,"seats":[{"id":"lifecycle-document-own",
        "name":"Owned profile independent", "mouse":"virtual://mouse/1", "keyboard":"virtual://keyboard/1",
        "target_pid":987001,"cursor":{"shape":"ring","badge":"OWN"},
        "output":{"buttons":[{"type":"legacy"}]*5,"keys":{"116":{"type":"disabled"}}}}]})
    profile_bytes = profile.read_bytes()
    evidence = {"result":"NOT ACCEPTED", "phase":"preferences-document-provider", "checks":{},
        "build_sha256":digest(image), "scope":"standalone MCP only; no native tray/shortcut/registry claim"}
    processes = []; client = None
    try:
        client = Client(image, prefs, missing_runtime, run/"denied.stderr.log")
        processes.append(client.process)
        tools = client.rpc("tools/list", {})["tools"]
        assert {t["name"] for t in tools} == base.EXPECTED_TOOLS | {"app_preferences"}
        assert all(t["inputSchema"].get("type")=="object" for t in tools)
        before = client.tool("routing_state")
        inspection = client.tool("app_preferences", {"operation":"inspect"})
        assert inspection["source"]=="standalone_document" and inspection["startup_registration"]
        defaults = inspection["preferences"]
        expected = {"version":1,"launch_minimized":False,"minimize_to_tray":False,"close_to_tray":False,
            "launch_at_login":False,"shortcuts":[{"action":"stop","key":"f11",
            "modifiers":{"control":True,"alt":True,"shift":True,"windows":False}}]}
        assert defaults == expected, defaults
        settings = {k:v for k,v in defaults.items() if k not in ("version","launch_at_login")}
        client.tool("app_preferences", {"operation":"configure","settings":settings}, error=True)
        assert not prefs.exists() and client.tool("routing_state")==before
        evidence["checks"]["default_inspection_and_explicit_write_denial"] = True
        client.close(); client = None

        client = Client(image, prefs, missing_runtime, run/"allowed.stderr.log", allow=True)
        processes.append(client.process)
        before = client.tool("routing_state")
        settings.update(launch_minimized=True,minimize_to_tray=True,close_to_tray=True)
        configured = client.tool("app_preferences", {"operation":"configure","settings":settings})
        assert configured["source"]=="standalone_document" and configured["startup_registration"]
        applied = configured["preferences"]
        expected.update(settings)
        assert applied==expected and json.loads(prefs.read_bytes())==expected
        saved = prefs.read_bytes()
        assert client.tool("app_preferences", {"operation":"inspect"})["preferences"]==expected
        invalid = []
        candidate = copy.deepcopy(settings);candidate["shortcuts"][0]["modifiers"]={"control":False,"alt":False,"shift":True,"windows":False};invalid.append(candidate)
        candidate = copy.deepcopy(settings);candidate["shortcuts"].append(copy.deepcopy(candidate["shortcuts"][0]));invalid.append(candidate)
        candidate = copy.deepcopy(settings);extra=copy.deepcopy(candidate["shortcuts"][0]);extra["action"]="pause_resume";candidate["shortcuts"].append(extra);invalid.append(candidate)
        candidate = copy.deepcopy(settings);candidate["launch_at_login"]=True;invalid.append(candidate)
        for candidate in invalid:
            client.tool("app_preferences", {"operation":"configure","settings":candidate}, error=True)
            assert prefs.read_bytes()==saved
            assert client.tool("app_preferences", {"operation":"inspect"})["preferences"]==expected
            assert client.tool("routing_state")==before
        replacement = copy.deepcopy(settings)
        replacement["close_to_tray"] = not settings["close_to_tray"]
        with deny_owned_file_replacement(prefs):
            client.tool("app_preferences", {"operation":"configure","settings":replacement}, error=True)
            assert prefs.read_bytes()==saved
            assert client.tool("app_preferences", {"operation":"inspect"})["preferences"]==expected
            assert client.tool("routing_state")==before
            assert not list(run.glob(".*.tmp.*")), "Failed replace leaked its real temporary file"
        evidence["checks"]["actual_windows_destination_replace_failure_atomicity"] = True
        # Exercise the actual file-loading boundary, without injecting a codec
        # hook or flooding a tool response. The process must remain usable once
        # the owned damaged file has been restored by this fixture.
        broken_documents = {
            "invalid_version":json.dumps({**expected,"version":2}).encode(),
            "invalid_json":b"{broken owned preferences",
            "oversized_bytes":b" "*(64*1024+1),
        }
        file_rejections = {}
        for label, broken in broken_documents.items():
            prefs.write_bytes(broken)
            client.tool("app_preferences", {"operation":"inspect"}, error=True)
            client.tool("app_preferences", {"operation":"configure","settings":settings}, error=True)
            assert prefs.read_bytes()==broken and client.tool("routing_state")==before
            file_rejections[label]={"bytes":len(broken),"sha256":hashlib.sha256(broken).hexdigest()}
            prefs.write_bytes(saved)
            assert client.tool("app_preferences", {"operation":"inspect"})["preferences"]==expected
        evidence["rejected_existing_document_boundaries"] = file_rejections
        assert profile.read_bytes()==profile_bytes
        assert not list(missing_runtime.iterdir()) and not list(run.glob(".*.tmp.*"))
        evidence["checks"]["acknowledged_complete_roundtrip_invalid_atomicity_and_profile_independence"] = True
        evidence["tool_count"] = len(tools)
        evidence["preferences_sha256"] = digest(prefs)
        evidence["profile_sha256"] = digest(profile)
        evidence["result"] = "PASS"
    except Exception as error:
        evidence["failure"] = repr(error)
        raise
    finally:
        if client:
            try: client.close()
            except Exception as cleanup:
                evidence["cleanup_error"] = repr(cleanup)
                if client.process.poll() is None:client.process.kill();client.process.wait(timeout=10)
                evidence["result"]="NOT ACCEPTED"
        evidence["owned_processes_closed"] = all(p.poll() is not None for p in processes)
        put(run/"evidence.json", evidence)
        print(f"{evidence['result']}: {run}", flush=True)
    return run

def standalone_handoff(args):
    """One changed terminal-ordering check, run only after all GUI apps Quit."""
    from ctypes import wintypes
    base=load_module("lifecycle_handoff_mcp","test-mcp.py")
    hotkeys=load_module("lifecycle_handoff_hotkeys","owned-app-hotkeys.py")
    Client=client_type(base)
    run=args.runtime.resolve()/time.strftime("acceptance-app-lifecycle-handoff-%Y%m%d-%H%M%S")
    run.mkdir(parents=True)
    capture_harness(run)
    image=run/"control64.exe";shutil.copy2(args.executable,image)
    evidence={"result":"NOT ACCEPTED","phase":"standalone-immediate-stop-start",
        "build_sha256":digest(image),"scope":"Two fresh own x64 target processes; one virtual key family; ordinary native pump"}
    processes=[];client=None
    try:
        for index in range(2):
            processes.append(subprocess.Popen([str(image),"probe","--automation","--report",str(run/f"probe-{index}.json")],
                creationflags=subprocess.CREATE_NO_WINDOW))
        client=Client(image,run/"preferences.json",args.runtime.resolve(),run/"mcp.stderr.log")
        pids=[p.pid for p in processes]
        inventory=base.eventually(lambda:client.tool("routing_windows"),lambda value:
            all(any(w["pid"]==pid and w["title"].startswith("ProtoInputMultiPlex Automation Test")for w in value["windows"])for pid in pids),30)
        windows=[next(w for w in inventory["windows"]if w["pid"]==pid and w["title"].startswith("ProtoInputMultiPlex Automation Test"))for pid in pids]
        for index,window in enumerate(windows):
            assert base.user32.SetWindowPos(window["hwnd"],wintypes.HWND(-1),30+index*460,40,420,360,0x10)
        inventory=client.tool("routing_windows")
        windows=[next(w for w in inventory["windows"]if w["pid"]==pid and w["title"].startswith("ProtoInputMultiPlex Automation Test"))for pid in pids]
        reports=lambda:[json.loads((run/f"probe-{i}.json").read_bytes())for i in range(2)]
        def start(index):
            return client.tool("routing_start",{"users":[f"Owned handoff {index}"],"allowed_pids":[pids[index]],
                "mode":"concurrent","multi_keyboard":True})
        def focus_hold(index):
            bounds=windows[index]["client"]
            client.tool("routing_input",{"action":"move_to","user":0,"x":bounds["left"]+120,"y":bounds["top"]+100})
            for down in (True,False):
                client.tool("routing_input",{"action":"button","user":0,"button":"primary","pressed":down})
            held=client.tool("routing_input",{"action":"key","user":0,"key":"f5","pressed":True})
            base.eventually(reports,lambda values:116 in values[index]["held_keys"])
            return held
        first=start(0);first_held=focus_hold(0)
        overlays=[u["overlay"]["hwnd"]for u in first_held["users"]]
        first_stop=client.tool("routing_stop")
        stopped_at=time.monotonic()
        assert all(not base.user32.IsWindow(hwnd)for hwnd in overlays)
        # The very next MCP operation is Start: no sleep, retry, registration
        # probe or target reuse can conceal a lingering session-owned shortcut.
        second=start(1)
        restarted_at=time.monotonic()
        second_held=focus_hold(1)
        second_overlays=[u["overlay"]["hwnd"]for u in second_held["users"]]
        second_stop=client.tool("routing_stop")
        assert all(not base.user32.IsWindow(hwnd)for hwnd in second_overlays)
        delivered=base.eventually(reports,lambda values:all(len(v["key_events"])>=2 for v in values))
        for report in delivered:
            assert [(e["vk"],e["pressed"])for e in report["key_events"]]==[(116,True),(116,False)],"Plain native UP missing or duplicated"
        with hotkeys.OwnedHotkeys() as fixture:
            registration=fixture.probe(0x7A,True)
        evidence.update(result="PASS",pids=pids,first_stop=first_stop,second_start=second,second_stop=second_stop,
            next_start_elapsed_seconds=restarted_at-stopped_at,native_key_events=[r["key_events"]for r in delivered],
            final_F11_registration_probe=registration,owned_native_overlay_teardown=True)
    except Exception as error:
        evidence["failure"]=repr(error)
        raise
    finally:
        if client:
            try:client.close()
            except Exception as cleanup:
                evidence["cleanup_error"]=repr(cleanup);evidence["result"]="NOT ACCEPTED"
                if client.process.poll()is None:client.process.kill();client.process.wait(timeout=10)
        for process in processes:
            if process.poll()is None:process.terminate()
            process.wait(timeout=10)
        evidence["owned_processes_closed"]=all(p.poll()is not None for p in processes)and(not client or client.process.poll()is not None)
        put(run/"evidence.json",evidence)
        print(f"{evidence['result']}: {run}",flush=True)
    return run

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--ready", action="store_true")
    parser.add_argument("--phase", choices=("documents","handoff"), default="documents")
    args=parser.parse_args()
    assert args.ready, "Await root frozen-image READY/exclusive test slot before launching"
    (document_group if args.phase=="documents" else standalone_handoff)(args)

if __name__=="__main__":main()
