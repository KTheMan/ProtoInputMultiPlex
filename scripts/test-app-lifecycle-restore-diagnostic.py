"""Two-action owned offline Hide/native-menu-Restore diagnostic, never a waiver."""
import argparse
import copy
import ctypes
import hashlib
import importlib.util
from pathlib import Path
import shutil
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready", action="store_true")
    for name in ("executable", "x86-executable", "runtime", "prefix"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    assert args.ready
    spec = importlib.util.spec_from_file_location("restore_diagnostic_lifecycle", Path(__file__).with_name("test-app-lifecycle-ui.py"))
    lifecycle = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(lifecycle)
    Gate, ui, hotkeys, geometry = lifecycle.gate_type()
    gate = Gate(args, "app-lifecycle-restore-diagnostic", offline=True, count=0)
    seed = ui.read_json(args.prefix / "owned preferences.json")
    seed.update(launch_minimized=False, minimize_to_tray=True, close_to_tray=True,
                launch_at_login=False, appearance_mode="dark", zoom_percent=100)
    assert len(seed["shortcuts"]) == 7
    retained = None

    def state(stage):
        hwnd = retained
        pid = lifecycle.wintypes.DWORD()
        thread = geometry.user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        rect = geometry.Rect()
        valid_rect = bool(geometry.user32.GetClientRect(hwnd, ctypes.byref(rect)))
        files = {}
        for name in ("ui.progress.json", "ui.ready.json"):
            path = gate.run / name
            try:
                stat = path.stat()
                content = ui.read_json(path)
                files[name] = {"mtime_ns": stat.st_mtime_ns, "bytes": stat.st_size, "value": content}
            except (OSError, ValueError) as error:
                files[name] = {"read_error": repr(error)}
        observed = {"at_monotonic": time.monotonic(), "retained_hwnd": hwnd,
            "IsWindow": bool(ui.user32.IsWindow(hwnd)),
            "IsWindowVisible": bool(geometry.user32.IsWindowVisible(hwnd)),
            "IsIconic": bool(geometry.user32.IsIconic(hwnd)),
            "native_pid": pid.value, "native_thread": thread,
            "GetClientRect_succeeded": valid_rect,
            "client_pixels": [rect.right-rect.left, rect.bottom-rect.top] if valid_rect else None,
            "ui_files": files}
        assert not observed["IsWindow"] or pid.value == gate.gui.pid
        gate.evidence.setdefault("retained_window_timeline", {})[stage] = observed
        gate.save()
        return observed

    def before(g):
        nonlocal retained
        hwnd = g.main_hwnd()
        if not hwnd or not g.lifecycle_hwnd():
            return False
        retained = hwnd
        if not g.screenshot("pre-hide-owned-home"):
            return False
        observed = state("pre_hide")
        assert observed["IsWindowVisible"] and not observed["IsIconic"]
        assert not (g.run / "ui.state.json").exists()
        return True

    def verify(name, sample):
        state("post_hide_immediate")
        hidden = ui.eventually(lambda: state("post_hide"),
            lambda s: s["IsIconic"] or not s["IsWindowVisible"])
        gate.evidence["hide_observed"] = hidden
        result = gate.native_menu("Open ProtoInputMultiPlex")
        # Save actual named-item MSAA result BEFORE waiting on GUI restoration.
        gate.evidence["actual_MSAA_restore_result"] = result
        gate.save()
        state("post_MSAA_immediate")
        try:
            restored = ui.eventually(lambda: state("post_MSAA_poll"),
                lambda s: s["IsWindow"] and s["IsWindowVisible"] and not s["IsIconic"])
            gate.evidence["restore_effect"] = {"visible_unminimized": True, "window": restored}
        except AssertionError as error:
            gate.evidence["restore_effect"] = {"visible_unminimized": False, "timeout": repr(error)}
            state("restore_timeout")
        assert not (gate.run / "ui.state.json").exists()
        gate.save()
        return True

    def after(g):
        # End the diagnostic only after retaining all failure metadata. There
        # are no attached targets/holds. No ShowWindow or substitute Restore.
        g.expected_exit = True
        state("before_cleanup")
        g.evidence["cleanup"] = "Owned offline process termination after the two diagnostic actions; no normal-shutdown or Restore-success claim."
        g.gui.terminate()
        g.gui.wait(timeout=10)
        return True

    try:
        source = gate.run / "runner-source"
        source.mkdir()
        shutil.copy2(Path(__file__), source / Path(__file__).name)
        gate.evidence["scope"] = "Offline x64 GUI only; no workers/targets/DLLs, exact two diagnostic actions: actual HomeHide and actual native MSAA Open."
        steps = [ui.step("hide", "Hide")]
        steps[0]["wait_ms"] = 12000
        gate.execute({"version": 1, "concurrent": True, "seats": []}, copy.deepcopy(seed),
                     steps, verify, before, after)
        gate.evidence["result"] = "DIAGNOSTIC COMPLETE"
        gate.evidence["feature_verdict"] = "PASS" if gate.evidence["restore_effect"]["visible_unminimized"] else "NOT ACCEPTED"
        gate.save()
    finally:
        gate.close()
        lifecycle.remove_startup_key(gate.startup_key)


if __name__ == "__main__":
    main()
