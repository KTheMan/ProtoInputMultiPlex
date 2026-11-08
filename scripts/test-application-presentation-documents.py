"""Focused appearance document acceptance; AST-only until explicit --ready.

No routing worker, registry access, GUI, target process or DLL is needed. Earlier
preference-provider and permission checks are retained rather than replayed.
"""
import argparse
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import time


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready", action="store_true")
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    args = parser.parse_args()
    assert args.ready, "Await frozen source/images and exclusive acceptance slot"
    lifecycle = load("presentation_lifecycle", "test-app-lifecycle-acceptance.py")
    base = load("presentation_mcp", "test-mcp.py")
    Client = lifecycle.client_type(base)
    run = args.runtime.resolve() / time.strftime("acceptance-application-presentation-documents-%Y%m%d-%H%M%S")
    run.mkdir(parents=True)
    image = run / "control64.exe"
    shutil.copy2(args.executable, image)
    source = run / "runner-source"
    source.mkdir()
    for file in (Path(__file__), Path(__file__).with_name("test-app-lifecycle-acceptance.py"),
                 Path(__file__).with_name("test-mcp.py")):
        shutil.copy2(file, source / file.name)
    missing = run / "missing-runtime"
    missing.mkdir()
    prefs = run / "preferences.json"
    legacy = {"version": 1, "launch_minimized": False, "minimize_to_tray": False,
              "close_to_tray": False, "launch_at_login": False, "shortcuts": []}
    lifecycle.put(prefs, legacy)
    evidence = {"result": "NOT ACCEPTED", "build_sha256": lifecycle.digest(image),
                "checks": {}, "scope": "Standalone in-memory/file app appearance only; no GUI/native/registry claim"}
    client = None
    process = None
    try:
        client = Client(image, prefs, missing, run / "mcp.stderr.log", allow=True)
        process = client.process
        tools = client.rpc("tools/list", {})["tools"]
        assert {t["name"] for t in tools} == base.EXPECTED_TOOLS | {"app_preferences"}
        schema = next(t["inputSchema"] for t in tools if t["name"] == "app_preferences")
        assert schema.get("type") == "object"
        before = client.tool("routing_state")
        inspected = client.tool("app_preferences", {"operation": "inspect"})["preferences"]
        expected = {**legacy, "appearance_mode": "dark", "zoom_percent": 100}
        assert inspected == expected
        assert json.loads(prefs.read_bytes()) == legacy, "Read-only inspection rewrote legacy preferences"
        settings = {k: v for k, v in legacy.items() if k not in ("version", "launch_at_login")}

        def configure(candidate):
            result = client.tool("app_preferences", {"operation": "configure", "settings": candidate})
            assert result["source"] == "standalone_document"
            return result["preferences"]

        for zoom in (50, 200, 75):
            candidate = {**settings, "appearance_mode": "light", "zoom_percent": zoom}
            expected.update(appearance_mode="light", zoom_percent=zoom)
            assert configure(candidate) == expected == json.loads(prefs.read_bytes())
        assert configure(settings) == expected, "Old-shape Configure reset current appearance"
        assert configure({**settings, "appearance_mode": "dark"}) == {**expected, "appearance_mode": "dark"}
        expected["appearance_mode"] = "dark"
        evidence["checks"]["legacy_defaults_valid_endpoints_and_independent_omission_merge"] = True
        saved = prefs.read_bytes()
        candidates = [{**settings, "zoom_percent": zoom} for zoom in (49, 201, 76)]
        candidates += [{**settings, "appearance_mode": "unknown-owned-theme"},
                       {**settings, "appearance_mode": "light", "launch_at_login": True}]
        for candidate in candidates:
            client.tool("app_preferences", {"operation": "configure", "settings": candidate}, error=True)
            assert prefs.read_bytes() == saved
            assert client.tool("app_preferences", {"operation": "inspect"})["preferences"] == expected
            assert client.tool("routing_state") == before
        assert not list(missing.iterdir())
        evidence["checks"]["invalid_zoom_theme_and_startup_field_preserve_exact_document_and_session"] = True
        evidence["preferences_sha256"] = hashlib.sha256(saved).hexdigest()
        evidence["tool_count"] = len(tools)
        evidence["result"] = "PASS"
    except Exception as error:
        evidence["failure"] = repr(error)
        raise
    finally:
        if client:
            try:
                client.close()
            except Exception as error:
                evidence["cleanup_error"] = repr(error)
                evidence["result"] = "NOT ACCEPTED"
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=10)
        evidence["owned_processes_closed"] = process is None or process.poll() is not None
        lifecycle.put(run / "evidence.json", evidence)
        print(f"{evidence['result']}: {run}", flush=True)


if __name__ == "__main__":
    main()
