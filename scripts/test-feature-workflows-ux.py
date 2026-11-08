"""Finite Output Rules, profile persistence and navigation UX acceptance.

Preparation only until root freezes a build and explicitly hands off the
exclusive runtime slot. This reuses the owned lifecycle UI driver and product
frames; it does not repeat the existing Output Rules native/backend suite.
"""
import argparse
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import time


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def read_json(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def step(ui, identifier, button, fields=None, *, key=None, disabled=False):
    value = ui.step(identifier, button, fields, disabled=disabled)
    if key is not None:
        value["key"] = key
    return value


def field_free_fences(ui, steps, identifiers, button):
    """Separate a stateful step from following fields while its snapshot is sampled."""
    fenced = []
    for target in steps:
        fenced.append(target)
        if target["id"] in identifiers:
            fenced.append(step(ui, target["id"] + "-field-fence", button, disabled=True))
    return fenced


def own_profile(tag="feature-ux"):
    return {
        "version": 1,
        "concurrent": True,
        "desktop": {"mode": "concurrent", "multi_keyboard": True, "allowed_pids": []},
        "replication": {"version": 1, "edges": []},
        "seats": [
            {
                "id": f"{tag}-{suffix}",
                "name": f"Owned {name}",
                "mouse": f"virtual://mouse/{index + 1}",
                "keyboard": f"virtual://keyboard/{index + 1}",
                "target_pid": 0,
                "cursor": {"shape": "ring" if index == 0 else "crosshair", "badge": name},
                "output": {"buttons": [{"type": "legacy"} for _ in range(5)], "keys": {}},
            }
            for index, (suffix, name) in enumerate((("a", "Alpha"), ("b", "Beta")))
        ],
    }


def preferences():
    return {
        "version": 1,
        "appearance_mode": "dark",
        "zoom_percent": 100,
        "launch_minimized": False,
        "minimize_to_tray": False,
        "close_to_tray": False,
        "launch_at_login": False,
        "shortcuts": [],
    }


def output_snapshot(gate):
    """Read the opt-in stable-identity OutputPage snapshot added for this gate."""
    value = read_json(gate.run / "ui.output.page.json")
    required = {"selected_user", "selected_identity", "committed", "drafts", "pending", "error", "status", "editor"}
    assert required <= value.keys(), f"OutputPage snapshot schema changed: {value.keys()}"
    return value


def profile_snapshot(gate):
    return read_json(gate.run / "ui.profile.json")


def navigation_snapshot(gate):
    value = read_json(gate.run / "ui.profile.navigation.json")
    assert {"selected_user", "page", "category"} <= value.keys(), value
    return value


def frame(gate, name, logical_size):
    """Capture one fresh product-owned frame at a measured logical client size."""
    from PIL import Image

    if not hasattr(gate, "frame_started"):
        gate.frame_started = {}
        gate.frames = {}
    if name not in gate.frame_started:
        hwnd = gate.main_hwnd()
        if not hwnd:
            return False
        gate.frame_started[name] = time.time()
        if logical_size is not None:
            gate.geometry.resize_owned_client(hwnd, logical_size)
        return False
    hwnd = gate.main_hwnd()
    if not hwnd:
        return False
    actual = gate.geometry.geometry(hwnd)
    if logical_size is not None and any(abs(a - b) > 2 for a, b in zip(actual["client_logical"], logical_size)):
        return False
    shot = gate.geometry.copy_owned_product_frame(
        gate.run / "ui.png", gate.run / f"{name}.png", hwnd, gate.frame_started[name]
    )
    if not shot:
        return False
    with Image.open(gate.run / f"{name}.png") as image:
        image.load()
        assert list(image.size) == actual["client_pixels"]
    gate.frames[name] = shot
    gate.evidence.setdefault("owned_product_frames", {})[name] = shot
    gate.save()
    return True


ROUTE_DESTINATIONS = [
        ("Application", ["Application preferences"]),
        ("Users & devices", ["Users & devices", "Profiles & backend"]),
        ("Automation", ["Macros & replication", "Macro Studio", "Replication Graph", "Virtual Replicas"]),
        ("Tools", ["Live preview", "Input lab", "Annotation", "Magnifier", "Output Rules"]),
        ("Agent access", ["Agent Access"]),
    ]


def route_steps(ui):
    """Every real category/destination, each followed by Back and re-entry."""
    steps = [step(ui, "routes-settings", "Settings", {"selected_user": "1"})]
    serial = 0
    for category, destinations in ROUTE_DESTINATIONS:
        for destination in destinations:
            serial += 1
            prefix = f"route-{serial:02d}"
            steps.extend([
                step(ui, prefix + "-category", f"Settings category {category}"),
                step(ui, prefix + "-destination", destination),
            ])
            if destination == "Macros & replication":
                # One bounded minimum-size frame checks the dense legacy form;
                # its native recording/replication semantics have separate gates.
                steps.extend([
                    step(ui, prefix + "-automation-refresh", "Refresh automation status", disabled=True),
                    step(ui, prefix + "-automation-minimum", "Apply replication", disabled=True),
                ])
            steps.extend([
                step(ui, prefix + "-back", "Back to Home"),
                step(ui, prefix + "-settings", "Settings"),
            ])
    return steps


def offline_steps(ui, gate):
    output_path = gate.run / "active profile.json"
    import_path = gate.run / "import profile.json"
    export_path = gate.run / "export profile.json"
    imported = own_profile("feature-import")
    imported["seats"][1]["output"]["buttons"][3] = {"type": "key", "key": 121}
    ui.put(import_path, imported)
    steps = [
        step(ui, "open-settings", "Settings", {"selected_user": "1"}),
        step(ui, "choose-tools", "Settings category Tools"),
        step(ui, "open-output", "Output Rules"),
        step(ui, "output-kind-key", "Apply output rules", {
            "output_user": "1", "output_button": "3", "output_kind": "key"
        }, disabled=True),
        step(ui, "output-invalid-key", "Save output rules to profile", {
            "output_user": "1", "output_key": "invalid-key"
        }, disabled=True),
        step(ui, "output-unadded-key-entry", "Save output rules to profile", {
            "output_user": "1", "output_key": "A", "output_source_key": "A", "output_destination_key": "B"
        }, disabled=True),
        step(ui, "output-unadded-apply-block", "Apply output rules", {"output_user": "1"}, disabled=True),
        step(ui, "output-add-key-mapping", "Add or replace key mapping", {"output_user": "1"}),
        step(ui, "output-valid-stage", "Apply output rules", {
            "output_user": "1", "output_key": "F9"
        }, disabled=True),
        step(ui, "output-switch-A", "Discard output changes", {"output_user": "0"}, disabled=True),
        step(ui, "output-switch-back-B", "Apply output rules", {"output_user": "1"}, disabled=True),
        step(ui, "output-leave", "Back to Home", {"selected_user": "1"}),
        step(ui, "output-return-settings", "Settings"),
        step(ui, "output-return-tools", "Settings category Tools"),
        step(ui, "output-return-page", "Output Rules"),
        step(ui, "output-return-check", "Apply output rules", disabled=True),
        step(ui, "keyboard-tab-discard", "Discard output changes", {"output_user": "1"}, key="Tab"),
        step(ui, "keyboard-enter-discard", "Discard output changes", {"output_user": "1"}, key="Enter"),
        step(ui, "output-discard", "Apply output rules", {"output_user": "1"}, disabled=True),
        step(ui, "output-keyboardless-block", "Save output rules to profile", {
            "output_user": "0", "output_button": "3", "output_kind": "key", "output_key": "F9"
        }, disabled=True),
        step(ui, "output-discard-keyboardless", "Discard output changes", {"output_user": "0"}),
        step(ui, "output-physical-block", "Save output rules to profile", {"output_user": "2"}, disabled=True),
        step(ui, "output-return-B-after-gates", "Apply output rules", {"output_user": "1"}, disabled=True),
        step(ui, "output-save-draft", "Save output rules to profile", {
            "output_user": "1", "output_button": "3", "output_kind": "key", "output_key": "F9"
        }),
        step(ui, "output-back-home", "Back to Home"),
        step(ui, "profiles-settings", "Settings"),
        step(ui, "profiles-users-category", "Settings category Users & devices"),
        step(ui, "profiles-page", "Profiles & backend", {"profile_path": str(output_path)}),
        step(ui, "profiles-minimum-frame", "Save profile", {"profile_path": str(output_path)}, key="Tab"),
        step(ui, "profiles-keyboard-save", "Save profile", {"profile_path": str(output_path)}, key="Enter"),
        step(ui, "open-import-disclosure", "Import / export"),
        step(ui, "export-profile", "Export profile", {"profile_transfer_path": str(export_path)}),
        step(ui, "import-profile", "Import profile", {"profile_transfer_path": str(import_path)}),
        step(ui, "open-recovery-disclosure", "Profile recovery"),
        step(ui, "undo-import", "Undo profile change"),
        step(ui, "reset-profile", "Reset profile"),
        step(ui, "undo-reset", "Undo profile change"),
        step(ui, "mutate-before-load", "Reset profile"),
        step(ui, "load-profile", "Load profile", {"profile_path": str(output_path)}),
        step(ui, "open-scope-disclosure", "Automatic targeting scope"),
    ]
    # The shared page observer sees each disclosure header as its exact label;
    # the four open states are checked by pixels/state below. Route navigation
    # follows in a separate finite tail on the same stopped GUI.
    steps = field_free_fences(ui, steps, {
        "open-output", "output-kind-key", "output-invalid-key", "output-unadded-key-entry",
        "output-unadded-apply-block", "output-add-key-mapping", "output-valid-stage",
        "output-switch-A", "output-switch-back-B", "output-return-page", "output-return-check",
        "keyboard-tab-discard", "keyboard-enter-discard", "output-discard",
        "output-keyboardless-block", "output-discard-keyboardless", "output-physical-block",
        "output-return-B-after-gates", "output-save-draft",
    }, "Apply output rules")
    steps.extend([step(ui, "profiles-back-home", "Back to Home")])
    steps.extend(route_steps(ui))
    return steps, output_path, import_path, export_path


def finish_lifecycle_gate(gate, lifecycle):
    """Attempt exact owned cleanup, remove the private test key, and audit only our copied images."""
    failures = []
    try:
        gate.close()
    except Exception as error:
        failures.append("gate.close: " + repr(error))
    finally:
        try:
            lifecycle.remove_startup_key(gate.startup_key)
        except Exception as error:
            failures.append("startup key removal: " + repr(error))
    try:
        key_removed = lifecycle.startup_value(gate.startup_key) is None
        import psutil
        owned_images = {os.path.normcase(str(Path(image).resolve())) for image in gate.images}
        remaining = []
        for process in psutil.process_iter(["pid", "exe", "cmdline"]):
            try:
                info = process.info
                executable = info.get("exe")
                if not executable or os.path.normcase(str(Path(executable).resolve())) not in owned_images:
                    continue
                command = " ".join(info.get("cmdline") or []).lower()
                role = "session-worker" if "session-worker" in command else (
                    "gui" if " gui " in f" {command} " else (
                        "probe" if " probe " in f" {command} " else "owned-image-process"))
                remaining.append({"pid": info["pid"], "image": Path(executable).name, "role": role})
            except (psutil.NoSuchProcess, psutil.ZombieProcess):
                continue
            except psutil.AccessDenied as error:
                failures.append("process audit access denied: " + repr(error))
        audit = {"startup_key_removed": key_removed, "owned_binary_processes_closed": not remaining,
                 "remaining_exact_owned_image_processes": remaining}
        if not key_removed:
            failures.append("private startup test key remains")
        if remaining:
            failures.append("owned-image processes remain")
    except Exception as error:
        audit = {"startup_key_removed": False, "owned_binary_processes_closed": False,
                 "audit_error": repr(error)}
        failures.append("cleanup audit: " + repr(error))
    audit["cleanup_errors"] = failures
    gate.evidence["cleanup_audit"] = audit
    if failures:
        gate.evidence["result"] = "NOT ACCEPTED"
        gate.evidence["cleanup_failure"] = failures
    try:
        gate.save()
    except Exception:
        pass
    return audit


def make_gate(lifecycle, args, label, executable, *, offline, count):
    Base, ui, hotkeys, geometry = lifecycle.gate_type()

    class Gate(Base):
        def __init__(self):
            run_args = copy.copy(args)
            run_args.executable = executable
            super().__init__(run_args, label, offline=offline, count=count)
            self.geometry = geometry

        @staticmethod
        def assert_event_contract(steps, events):
            assert len(events) <= len(steps)
            for target, event in zip(steps, events):
                if target.get("expect_disabled"):
                    assert event.get("clicked") is False and event.get("observed_disabled") is True, event
                elif target.get("key"):
                    assert event.get("key") == target["key"] and event.get("focused") is True, event
                    if target["key"] == "Enter":
                        assert event.get("clicked") is True, event
                else:
                    assert event.get("button") == target["button"] and event.get("clicked") is True, event

    return Gate, ui, hotkeys, geometry


def offline_phase(args, executable, label):
    lifecycle = module("feature_ux_lifecycle", "test-app-lifecycle-ui.py")
    Gate, ui, hotkeys, geometry = make_gate(lifecycle, args, label, executable, offline=True, count=0)
    gate = Gate()
    profile = own_profile()
    profile["seats"][0]["keyboard"] = None
    physical = copy.deepcopy(profile["seats"][0])
    physical.update(id="feature-ux-physical", name="Synthetic physical seat", mouse="physical://unattached-mouse", keyboard="virtual://keyboard/3")
    profile["seats"].append(physical)
    profile_path = gate.run / "active profile.json"
    profile_path.write_text(json.dumps(profile, ensure_ascii=False, indent=2), encoding="utf-8")
    initial_bytes = profile_path.read_bytes()
    prefs = preferences()
    steps, active_path, import_path, export_path = offline_steps(ui, gate)
    original = copy.deepcopy(profile)
    route_pages = {
        "Application preferences": "AppPreferences", "Users & devices": "Users",
        "Profiles & backend": "Profiles", "Macros & replication": "Automation",
        "Macro Studio": "MacroStudio", "Replication Graph": "Replication",
        "Virtual Replicas": "VirtualUsers", "Live preview": "Preview",
        "Input lab": "Lab", "Annotation": "Annotation", "Magnifier": "Magnifier",
        "Output Rules": "Output", "Agent Access": "Agent",
    }
    route_expectations = {}
    route_number = 0
    for route_category, destinations in ROUTE_DESTINATIONS:
        for destination in destinations:
            route_number += 1
            prefix = f"route-{route_number:02d}"
            route_expectations[prefix + "-category"] = ("Settings", route_category)
            route_expectations[prefix + "-destination"] = (route_pages[destination], route_category)
            route_expectations[prefix + "-back"] = ("Home", route_category)
            route_expectations[prefix + "-settings"] = ("Settings", route_category)
    last_output = None
    last_navigation = None
    saved_profile_bytes = None
    saved_profile_snapshot = None

    def verify(name, sample):
        nonlocal last_output, last_navigation, saved_profile_bytes, saved_profile_snapshot
        profile_state = profile_snapshot(gate)
        sample["profile_state"] = profile_state
        sample["navigation"] = last_navigation = navigation_snapshot(gate)

        def wait_profile(predicate, timeout=8):
            state = ui.eventually(lambda: profile_snapshot(gate), predicate, timeout)
            sample["profile_state"] = state
            return state

        def wait_output(predicate, timeout=8):
            nonlocal last_output
            page = ui.eventually(lambda: output_snapshot(gate), predicate, timeout)
            sample["output_page"] = last_output = page
            return page

        if (gate.run / "ui.state.json").exists():
            raise AssertionError("Offline workflow unexpectedly created a routing worker")
        if name == "open-output":
            sample["output_page"] = last_output = output_snapshot(gate)
            assert last_output["selected_identity"] == original["seats"][1]["id"], (
                "Entering Output did not follow the Home-selected user", last_output
            )
        if name.startswith("output-"):
            sample["output_page"] = last_output = output_snapshot(gate)
        if name == "output-invalid-key" and not frame(gate, "output-normal", [680, 620]):
            return False
        if name == "output-valid-stage" and not frame(gate, "output-minimum", [320, 360]):
            return False
        if name == "profiles-page":
            sample["profile_state"] = profile_state
            if not frame(gate, "profiles-normal", [680, 620]):
                return False
        if name == "profiles-minimum-frame":
            hwnd = gate.main_hwnd()
            if not hwnd:
                return False
            if not getattr(gate, "profiles_minimum_resize_requested", False):
                gate.geometry.resize_owned_client(hwnd, [320, 360])
                gate.profiles_minimum_resize_requested = True
                return False
            actual = gate.geometry.geometry(hwnd)["client_logical"]
            if any(abs(a - b) > 2 for a, b in zip(actual, [320, 360])):
                return False
        if name in ("open-recovery-disclosure", "open-scope-disclosure"):
            sample["profile_disclosures"] = [
                "Profile recovery", "Automatic targeting scope"
            ]
        if name == "output-invalid-key":
            page = sample["output_page"]
            assert page["error"] or page["editor"].get("parse_error"), page
            assert page["committed"][1][1]["buttons"][3] == {"type": "legacy"}
            assert page["drafts"][1][1]["buttons"][3] == {"type": "key", "key": 65}, "Invalid text changed the last valid staged value"
            assert profile_path.read_bytes() == initial_bytes
        if name == "output-unadded-key-entry":
            page = sample["output_page"]
            assert page["editor"]["source_key_text"] == "A"
            assert page["editor"]["destination_key_text"] == "B"
            assert page["drafts"][1][1]["keys"] == {}, "Unadded key text changed the staged map"
            assert profile_path.read_bytes() == initial_bytes
        if name == "output-unadded-apply-block":
            page = sample["output_page"]
            assert page["editor"]["source_key_text"] == "A"
            assert page["editor"]["destination_key_text"] == "B", "A blocked commit erased an unadded valid mapping"
            assert page["drafts"][1][1]["keys"] == {}
        if name == "output-add-key-mapping":
            page = wait_output(lambda value: value["drafts"][1][1]["keys"] == {"65": {"type": "key", "key": 66}}
                               and value["editor"]["source_key_text"] == ""
                               and value["editor"]["destination_key_text"] == "")
            assert page["editor"]["source_key_text"] == ""
            assert page["editor"]["destination_key_text"] == ""
            assert page["drafts"][1][1]["keys"] == {"65": {"type": "key", "key": 66}}, page["drafts"][1]
        if name == "output-valid-stage":
            page = sample["output_page"]
            assert page["drafts"] != page["committed"], "Valid field edit was not staged"
            assert page["drafts"][1][0] == original["seats"][1]["id"]
            assert page["drafts"][1][1]["buttons"][3] == {"type": "key", "key": 120}
            assert page["drafts"][0] == page["committed"][0], "Editing B changed A"
        if name == "output-switch-A":
            page = sample["output_page"]
            assert page["selected_identity"] == original["seats"][0]["id"]
            assert page["drafts"][1][1]["buttons"][3] == {"type": "key", "key": 120}
        if name in ("output-switch-back-B", "output-return-check"):
            page = sample["output_page"]
            assert page["selected_identity"] == original["seats"][1]["id"]
            assert page["drafts"][1][1]["buttons"][3] == {"type": "key", "key": 120}
        if name == "output-return-page":
            page = sample["output_page"]
            assert page["selected_identity"] == original["seats"][1]["id"]
            assert page["drafts"][1][1]["buttons"][3] == {"type": "key", "key": 120}
            assert page["drafts"][1][1]["keys"] == {"65": {"type": "key", "key": 66}}
        if name == "output-discard":
            page = wait_output(lambda value: value["drafts"] == value["committed"])
            assert page["drafts"] == page["committed"], "Discard did not restore every committed user draft"
        if name == "output-discard-keyboardless":
            page = wait_output(lambda value: value["drafts"][0] == value["committed"][0])
            assert page["drafts"][0] == page["committed"][0]
        if name == "output-keyboardless-block":
            page = sample["output_page"]
            assert page["selected_identity"] == original["seats"][0]["id"]
            assert page["drafts"][0][1]["buttons"][3] == {"type": "key", "key": 120}
            assert page["drafts"][0] != page["committed"][0]
        if name == "output-physical-block":
            page = sample["output_page"]
            assert page["selected_identity"] == "feature-ux-physical"
        if name == "output-return-B-after-gates":
            assert sample["output_page"]["selected_identity"] == original["seats"][1]["id"]
        if name == "output-save-draft":
            profile_state = wait_profile(lambda value:
                value["profile"]["seats"][1]["output"]["buttons"][3] == {"type": "key", "key": 120}
                and value["profile"]["seats"][0]["output"] == original["seats"][0]["output"])
            current = profile_state["profile"]
            assert current["seats"][1]["output"]["buttons"][3] == {"type": "key", "key": 120}
            assert current["seats"][0]["output"] == original["seats"][0]["output"]
            assert profile_path.read_bytes() == initial_bytes, "Profile-page edit unexpectedly wrote disk"
        if name == "profiles-keyboard-save":
            # Script receipts arrive before the next frame has necessarily
            # flushed the profile file. Wait for the concrete persisted state
            # instead of treating that brief write window as a product failure.
            saved_state = ui.eventually(
                lambda: {
                    "snapshot": profile_snapshot(gate),
                    "disk": json.loads(active_path.read_bytes()),
                },
                lambda value: value["disk"] == value["snapshot"]["profile"]
                and value["snapshot"]["status"].startswith("Saved "),
                8,
            )
            profile_state = saved_state["snapshot"]
            sample["profile_state"] = profile_state
            saved_profile_bytes = active_path.read_bytes()
            saved_profile_snapshot = copy.deepcopy(profile_state["profile"])
            if not frame(gate, "profiles-minimum", [320, 360]):
                return False
        if name == "export-profile":
            exported_state = ui.eventually(
                lambda: {"file": json.loads(export_path.read_bytes()), "snapshot": profile_snapshot(gate)},
                lambda value: value["file"] == value["snapshot"]["profile"]
                and value["snapshot"]["status"].startswith("Exported a validated snapshot"),
                8,
            )
            exported = exported_state["file"]
            sample["exported_profile"] = exported
            assert active_path.read_bytes() == saved_profile_bytes, "Export changed the active profile file"
        if name == "import-profile":
            profile_state = wait_profile(lambda value:
                value["profile"]["seats"][1]["id"] == "feature-import-b"
                and value["profile"]["seats"][1]["output"]["buttons"][3] == {"type": "key", "key": 121}
                and value["status"].startswith("Profile imported."))
            assert profile_state["profile"]["seats"][1]["output"]["buttons"][3] == {"type": "key", "key": 121}
            assert active_path.read_bytes() == saved_profile_bytes, "Import rewrote the previously saved active file"
        if name == "undo-import":
            profile_state = wait_profile(lambda value:
                value["profile"] == saved_profile_snapshot and value["undo_available"] is False
                and value["status"].startswith("Restored the previous in-memory profile."))
            assert profile_state["undo_available"] is False
            assert profile_state["profile"] == saved_profile_snapshot
            assert active_path.read_bytes() == saved_profile_bytes, "Undo Import changed the saved profile file"
        if name == "reset-profile":
            profile_state = wait_profile(lambda value: value["profile"]["seats"] == []
                                         and value["status"].startswith("Reset to an empty profile.")
                                         and active_path.exists())
            assert profile_state["profile"]["seats"] == [] and active_path.exists()
            assert active_path.read_bytes() == saved_profile_bytes, "Reset changed the saved profile file"
        if name == "undo-reset":
            profile_state = wait_profile(lambda value:
                value["profile"] == saved_profile_snapshot and value["undo_available"] is False
                and value["status"].startswith("Restored the previous in-memory profile."))
            assert profile_state["profile"] == saved_profile_snapshot
            assert profile_state["undo_available"] is False
            assert active_path.read_bytes() == saved_profile_bytes, "Undo Reset changed the saved profile file"
        if name == "mutate-before-load":
            profile_state = wait_profile(lambda value: value["profile"]["seats"] == []
                                         and value["status"].startswith("Reset to an empty profile."))
            assert profile_state["profile"]["seats"] == [], "Load precondition was not mutated"
            assert active_path.read_bytes() == saved_profile_bytes, "Pre-load Reset changed the saved profile file"
        if name == "load-profile":
            loaded = ui.eventually(
                lambda: {"snapshot": profile_snapshot(gate), "disk": json.loads(active_path.read_bytes())},
                lambda value: value["snapshot"]["profile"] == value["disk"] == saved_profile_snapshot
                and value["snapshot"]["status"].startswith("Profile loaded."),
                8,
            )
            profile_state = loaded["snapshot"]
            sample["profile_state"] = profile_state
            assert loaded["disk"] == profile_state["profile"] == saved_profile_snapshot
        if name in route_expectations:
            expected_page, expected_category = route_expectations[name]
            assert last_navigation["page"] == expected_page, (name, last_navigation)
            assert last_navigation["category"] == expected_category, (name, last_navigation)
            assert last_navigation["selected_user"] == 1, (name, last_navigation)
        if name == "route-12-destination":
            page = output_snapshot(gate)
            assert page["selected_identity"] == original["seats"][1]["id"], (
                "Settings → Output Rules lost the selected Home user", page
            )
        if name.endswith("-automation-refresh"):
            hwnd = gate.main_hwnd()
            if not hwnd:
                return False
            if not getattr(gate, "automation_minimum_resize_requested", False):
                gate.geometry.resize_owned_client(hwnd, [320, 360])
                gate.automation_minimum_resize_requested = True
            actual = gate.geometry.geometry(hwnd)
            sample["automation_pre_apply_client_logical"] = actual["client_logical"]
            if any(abs(actual_size - expected_size) > 2
                   for actual_size, expected_size in zip(actual["client_logical"], [320, 360])):
                return False
        if name.endswith("-automation-minimum"):
            if not frame(gate, "automation-minimum", [320, 360]):
                return False
        if name == "route-13-settings":
            assert last_navigation["page"] == "Settings" and last_navigation["category"] == "Agent access", last_navigation
            if not frame(gate, "settings-representative", [680, 620]):
                return False
        return True

    def after_controls(g):
        final = profile_snapshot(g)["profile"]
        ui.put(g.run / "final-profile.json", final)
        g.expected_exit = True
        assert geometry.user32.PostMessageW(g.main_hwnd(), 0x10, 0, 0)
        g.gui.wait(timeout=10)
        assert g.gui.returncode == 0
        return True

    try:
        provenance = gate.run / "runner-source"
        provenance.mkdir(exist_ok=True)
        shutil.copy2(Path(__file__), provenance / Path(__file__).name)
        gate.evidence["scope"] = "Stopped owned GUI/profile-document/navigation workflow only; no worker, DLL, MCP, native input or hardware"
        gate.evidence["preferences_sha256_before"] = hashlib.sha256(json.dumps(prefs, sort_keys=True).encode()).hexdigest()
        gate.execute(profile, prefs, steps, verify, lambda current: current.main_hwnd() is not None, after_controls)
        gate.evidence.update(result="PENDING INDEPENDENT VISUAL REVIEW", original_profile_sha256=digest(profile_path),
                             owned_product_frames=getattr(gate, "frames", {}))
        gate.save()
        return gate.run
    finally:
        finish_lifecycle_gate(gate, lifecycle)


def layout_phase(args, executable):
    """Two replacement minimum-width captures after the bounded layout corrections."""
    lifecycle = module("feature_ux_lifecycle", "test-app-lifecycle-ui.py")
    Gate, ui, hotkeys, geometry = make_gate(
        lifecycle, args, "feature-workflows-ux-layout", executable, offline=True, count=0
    )
    gate = Gate()
    profile = own_profile("feature-ux-layout")
    prefs = preferences()
    active_path = gate.run / "active profile.json"
    steps = [
        step(ui, "layout-profiles-settings", "Settings"),
        step(ui, "layout-profiles-category", "Settings category Users & devices"),
        step(ui, "layout-profiles-page", "Profiles & backend", {"profile_path": str(active_path)}),
        step(ui, "layout-profiles-focus-save", "Save profile", key="Tab"),
        step(ui, "layout-profiles-save", "Save profile", key="Enter"),
        # The same target forms a field-free fence while the Save result is sampled,
        # preventing the following Import disclosure from scrolling the page first.
        step(ui, "layout-profiles-save-focus-fence", "Save profile", key="Tab"),
        step(ui, "layout-profiles-home", "Back to Home"),
        step(ui, "layout-automation-settings", "Settings"),
        step(ui, "layout-automation-category", "Settings category Automation"),
        step(ui, "layout-automation-page", "Macros & replication"),
        step(ui, "layout-automation-refresh", "Refresh automation status", disabled=True),
        step(ui, "layout-automation-start-recording", "Start recording", disabled=True),
        step(ui, "layout-automation-stop-recording", "Stop recording", disabled=True),
        step(ui, "layout-automation-cancel-playback", "Cancel playback", disabled=True),
        step(ui, "layout-automation-play", "Play macro", disabled=True),
        step(ui, "layout-automation-from", "From user"),
        step(ui, "layout-automation-from-choice", "User 1: Owned Alpha"),
        step(ui, "layout-automation-to", "To user"),
        # Same-user selection is a harmless form choice that keeps Add disabled.
        step(ui, "layout-automation-to-choice", "User 1: Owned Alpha"),
        step(ui, "layout-automation-add", "Add replication edge", disabled=True),
        step(ui, "layout-automation-apply", "Apply replication", disabled=True),
    ]

    def verify(name, sample):
        if (gate.run / "ui.state.json").exists():
            raise AssertionError("Layout qualification unexpectedly created a routing worker")
        if name == "layout-profiles-page":
            hwnd = gate.main_hwnd()
            if not hwnd:
                return False
            if not getattr(gate, "profiles_minimum_resize_requested", False):
                gate.geometry.resize_owned_client(hwnd, [320, 360])
                gate.profiles_minimum_resize_requested = True
                return False
            actual = gate.geometry.geometry(hwnd)["client_logical"]
            return all(abs(value - wanted) <= 2 for value, wanted in zip(actual, [320, 360]))
        if name == "layout-profiles-save":
            saved = ui.eventually(
                lambda: {"snapshot": profile_snapshot(gate), "disk": json.loads(active_path.read_bytes())},
                lambda value: value["snapshot"]["profile"] == value["disk"]
                and value["snapshot"]["status"].startswith("Saved "),
                8,
            )
            sample["profile_state"] = saved["snapshot"]
            sample["saved_profile_sha256"] = digest(active_path)
            if not frame(gate, "profiles-minimum-qualified", [320, 360]):
                return False
            return True
        if name == "layout-automation-page":
            hwnd = gate.main_hwnd()
            if not hwnd:
                return False
            if not getattr(gate, "automation_minimum_resize_requested", False):
                gate.geometry.resize_owned_client(hwnd, [320, 360])
                gate.automation_minimum_resize_requested = True
                return False
            actual = gate.geometry.geometry(hwnd)["client_logical"]
            sample["automation_minimum_client_logical"] = actual
            return all(abs(value - wanted) <= 2 for value, wanted in zip(actual, [320, 360]))
        if name in {
            "layout-automation-refresh", "layout-automation-start-recording",
            "layout-automation-stop-recording", "layout-automation-cancel-playback",
            "layout-automation-play", "layout-automation-add", "layout-automation-apply",
        }:
            assert sample["event"].get("visible") is True, sample["event"]
        if name == "layout-automation-apply":
            if not frame(gate, "automation-minimum-qualified", [320, 360]):
                return False
            sample["automation_minimum_client_logical"] = gate.geometry.geometry(
                gate.main_hwnd()
            )["client_logical"]
        return True

    def after_controls(current):
        current.expected_exit = True
        hwnd = current.main_hwnd()
        assert hwnd and geometry.user32.PostMessageW(hwnd, 0x10, 0, 0)
        current.gui.wait(timeout=10)
        assert current.gui.returncode == 0
        return True

    try:
        gate.evidence["scope"] = (
            "Stopped owned GUI only; two 320x360 replacement frames for Profiles Save feedback and "
            "Automation action reachability; no worker, DLL, MCP, native input, or hardware"
        )
        if getattr(args, "prior_offline", None):
            prior = Path(args.prior_offline).resolve()
            previous_evidence = read_json(prior / "evidence.json")
            previous_cleanup = previous_evidence.get("cleanup_audit", {})
            assert len(previous_evidence.get("clicked_events", [])) == 115
            assert previous_evidence.get("result") == "PENDING INDEPENDENT VISUAL REVIEW" and not previous_evidence.get("failure"), previous_evidence.get("failure")
            assert previous_cleanup.get("startup_key_removed") is True
            assert previous_cleanup.get("owned_binary_processes_closed") is True
            assert not previous_cleanup.get("remaining_exact_owned_image_processes")
            assert not previous_cleanup.get("cleanup_errors")
            gate.evidence["prior_functional_run"] = {
                "path": str(prior), "result": previous_evidence.get("result"),
                "observations": len(previous_evidence["clicked_events"]),
                "reused_frames": [
                    {"name": name, "path": str(prior / f"{name}.png"),
                     "sha256": digest(prior / f"{name}.png")}
                    for name in ("output-normal", "output-minimum", "profiles-normal", "settings-representative")
                ],
            }
        gate.execute(profile, prefs, steps, verify, lambda current: current.main_hwnd() is not None, after_controls)
        gate.evidence["result"] = "PENDING INDEPENDENT VISUAL REVIEW"
        gate.save()
        return gate.run
    finally:
        finish_lifecycle_gate(gate, lifecycle)


def baseline_phase(args, executable):
    """Render-only baseline; this binary predates candidate snapshot controls."""
    lifecycle = module("feature_ux_lifecycle", "test-app-lifecycle-ui.py")
    Gate, ui, hotkeys, geometry = make_gate(
        lifecycle, args, "feature-workflows-ux-baseline", executable, offline=True, count=0
    )
    gate = Gate()
    profile = own_profile()
    prefs = preferences()
    output_path = gate.run / "baseline-profile.json"
    steps = [
        step(ui, "baseline-settings-output", "Settings", {"selected_user": "1"}),
        step(ui, "baseline-tools", "Settings category Tools"),
        step(ui, "baseline-output", "Output Rules"),
        step(ui, "baseline-output-min", "Apply output rules", {"output_user": "1"}, disabled=True),
        step(ui, "baseline-home", "Back to Home"),
        step(ui, "baseline-settings-profiles", "Settings"),
        step(ui, "baseline-users", "Settings category Users & devices"),
        step(ui, "baseline-profiles", "Profiles & backend", {"profile_path": str(output_path)}),
        step(ui, "baseline-profiles-min", "Save profile", {"profile_path": str(output_path)}),
    ]

    def verify(name, sample):
        if name == "baseline-output" and not frame(gate, "baseline-output-normal", [680, 620]):
            return False
        if name == "baseline-output-min" and not frame(gate, "baseline-output-minimum", [320, 360]):
            return False
        if name == "baseline-profiles" and not frame(gate, "baseline-profiles-normal", [680, 620]):
            return False
        if name == "baseline-profiles-min" and not frame(gate, "baseline-profiles-minimum", [320, 360]):
            return False
        return True

    def after_controls(g):
        g.expected_exit = True
        assert geometry.user32.PostMessageW(g.main_hwnd(), 0x10, 0, 0)
        g.gui.wait(timeout=10)
        assert g.gui.returncode == 0
        return True

    try:
        gate.evidence["scope"] = "Current-main render reference only; no candidate-only state assertions"
        gate.execute(profile, prefs, steps, verify, lambda current: current.main_hwnd() is not None, after_controls)
        gate.evidence.update(result="PENDING INDEPENDENT VISUAL REVIEW", owned_product_frames=getattr(gate, "frames", {}))
        gate.save()
        return gate.run
    finally:
        finish_lifecycle_gate(gate, lifecycle)


def live_phase(args, executable):
    """Bounded live Output/Agent overlap with owned lifecycle startup state."""
    lifecycle = module("feature_ux_lifecycle", "test-app-lifecycle-ui.py")
    connected_module = module("feature_ux_connected", "test-connected-agents-acceptance.py")
    client_module = module("feature_ux_mcp", "test-mcp.py")
    Gate, ui, hotkeys, geometry = make_gate(
        lifecycle, args, "feature-workflows-ux-live", executable, offline=False, count=0
    )

    class LiveGate(Gate):
        def __init__(self):
            super().__init__()
            self.count = 2
            self.ids = ["feature-ux-a", "feature-ux-b"]
            self.pids, self.points, self.clients, self.descriptor = [], [], [], None

        def fixtures(self):
            with self.images[0].open("rb") as stream:
                assert stream.read(2) == b"MZ"
                stream.seek(0x3C)
                stream.seek(struct.unpack("<I", stream.read(4))[0])
                assert stream.read(4) == b"PE\0\0"
                assert struct.unpack("<H", stream.read(2))[0] == 0x8664, "Live probes must be x64"
            for index in range(2):
                self.owned.append(subprocess.Popen(
                    [str(self.images[0]), "probe", "--automation", "--report", str(self.run / f"probe-{index}.json")],
                    creationflags=subprocess.CREATE_NO_WINDOW,
                ))
            self.pids = [process.pid for process in self.owned]
            discovery = ui.Client(self.images[0], self.args.runtime.resolve(), self.run / "discovery.stderr.log")
            try:
                tools = discovery.rpc("tools/list", {})["tools"]
                assert {tool["name"] for tool in tools} == client_module.EXPECTED_TOOLS
                inventory = ui.eventually(
                    lambda: discovery.tool("routing_windows"),
                    lambda value: all(any(window["pid"] == pid and window["title"].startswith(
                        connected_module._ui._native._studio._base.PREFIX)
                        for window in value["windows"]) for pid in self.pids), 60,
                )
                for index, pid in enumerate(self.pids):
                    window = next(item for item in inventory["windows"] if item["pid"] == pid and item["title"].startswith(
                        connected_module._ui._native._studio._base.PREFIX))
                    assert ui.user32.SetWindowPos(window["hwnd"], ui.wintypes.HWND(-1), 30 + index * 460, 40, 420, 360, 0x10)
                inventory = discovery.tool("routing_windows")
                windows = [next(item for item in inventory["windows"] if item["pid"] == pid and item["title"].startswith(
                    connected_module._ui._native._studio._base.PREFIX)) for pid in self.pids]
                self.points = [[item["client"]["left"] + 120, item["client"]["top"] + 100] for item in windows]
            finally:
                discovery.close()

        def find_owned_descriptor(self):
            files = ui.eventually(
                lambda: list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{self.gui.pid}-*.json")),
                lambda candidates: len(candidates) == 1, 10,
            )
            self.descriptor = files[0]

    gate = LiveGate()
    legacy = {"buttons": [{"type": "legacy"} for _ in range(5)], "keys": {}}
    apply_a = copy.deepcopy(legacy)
    apply_a["buttons"][3] = {"type": "key", "key": 116}
    draft_b = copy.deepcopy(legacy)
    draft_b["buttons"][3] = {"type": "disabled"}
    external_b = copy.deepcopy(legacy)
    external_b["buttons"][0] = {"type": "disabled"}
    output_path = gate.run / "ui.output.page.json"
    agent_path = gate.run / "ui.agent.json"
    step_ui = ui.step
    steps = [
        step_ui("live-start", "Start routing"),
        step_ui("live-settings", "Settings", {"selected_user": 1}), step_ui("live-tools", "Settings category Tools"),
        step_ui("live-input", "Input lab"),
        step_ui("live-legacy-click-A-down", "1 Hold click"), step_ui("live-legacy-click-A-up", "1 Release click"),
        step_ui("live-legacy-click-B-down", "2 Hold click"), step_ui("live-legacy-click-B-up", "2 Release click"),
        step_ui("live-hold-B", "Hold key", {"selected_user": 1, "key_name": "f6"}),
        step_ui("live-home-output", "Back to Home"), step_ui("live-settings-output", "Settings"),
        step_ui("live-tools-output", "Settings category Tools"), step_ui("live-output", "Output Rules"),
        step_ui("live-select-A", "Save output rules to profile", {"output_user": 0}, disabled=True),
        step_ui("live-kind-A", "Save output rules to profile", {"output_button": 3, "output_kind": "key"}, disabled=True),
        step_ui("live-apply-A", "Apply output rules", {"output_key": "F5"}),
        step_ui("live-select-B", "Save output rules to profile", {"output_user": 1}, disabled=True),
        step_ui("live-stage-B", "Save output rules to profile", {"output_button": 3, "output_kind": "disabled"}, disabled=True),
        step_ui("live-switch-A", "Save output rules to profile", {"output_user": 0}, disabled=True),
        step_ui("live-switch-back-B", "Save output rules to profile", {"output_user": 1}, disabled=True),
        step_ui("live-agent-home", "Back to Home"), step_ui("live-agent-settings", "Settings"),
        step_ui("live-agent-category", "Settings category Agent access"), step_ui("live-agent-page", "Agent Access"),
        step_ui("live-agent-default-fence", "Disable agent access", disabled=True),
        step_ui("live-enable-agent", "Enable agent access", {
            "agent_users": ",".join(gate.ids), "agent_pids": ",".join(map(str, gate.pids)),
            "agent_capture": "false", "agent_session_control": "true", "agent_app_preferences": "false",
        }),
        step_ui("live-output-home", "Back to Home"), step_ui("live-output-settings", "Settings"),
        step_ui("live-output-tools", "Settings category Tools"), step_ui("live-output-return", "Output Rules"),
        step_ui("live-connected-B-update", "Save output rules to profile", disabled=True),
        step_ui("live-release-home", "Back to Home"), step_ui("live-release-settings", "Settings"),
        step_ui("live-release-tools", "Settings category Tools"), step_ui("live-release-lab", "Input lab"),
        step_ui("live-release-F6", "Release key", {"selected_user": 1, "key_name": "f6"}),
        step_ui("live-stop-home", "Back to Home"), step_ui("live-stop", "Stop routing"),
    ]
    steps = field_free_fences(ui, steps, {
        "live-output", "live-select-A", "live-kind-A", "live-apply-A", "live-select-B",
        "live-stage-B", "live-switch-A", "live-switch-back-B",
    }, "Save output rules to profile")
    handles = []
    connected = {"done": False, "summary": None}

    def profile_for_fixtures():
        profile = own_profile()
        profile["desktop"]["allowed_pids"] = list(gate.pids)
        for index, seat in enumerate(profile["seats"]):
            seat["id"] = gate.ids[index]
            seat["target_pid"] = gate.pids[index]
            seat["desktop_start"] = gate.points[index]
            seat["cursor"]["lock_window"] = False
        return profile

    def page():
        return read_json(output_path)

    def held_f6():
        reports = gate.probes()
        return len(reports) == 2 and 117 not in reports[0].get("held_keys", []) and 117 in reports[1].get("held_keys", [])

    def released_f6():
        reports = gate.probes()
        return len(reports) == 2 and all(117 not in report.get("held_keys", []) for report in reports)

    def settings_match(snapshot, user, settings):
        return next((entry.get("settings") for entry in snapshot.get("users", [])
                     if entry.get("user") == user), None) == settings

    def connected_update():
        gate.find_owned_descriptor()
        client = connected_module.ConnectedMcp(gate.images[0], gate.descriptor, gate.run / "connected.stderr.log")
        gate.clients.append(client)
        try:
            tools = client.rpc("tools/list", {})["tools"]
            assert {tool["name"] for tool in tools} == client_module.EXPECTED_TOOLS
            assert all(tool["inputSchema"]["type"] == "object" for tool in tools)
            reply = client.tool("routing_output", {"operation": "configure", "user": 1, "settings": external_b})
            assert reply.get("last_error") is None, reply
            reply_output = reply.get("output") or reply
            assert settings_match(reply_output, 1, external_b), reply
            state = client.tool("routing_state")
            assert state.get("last_error") is None and settings_match(state.get("output") or {}, 1, external_b), state
            connected["summary"] = {"tool": "routing_output", "operation": "configure", "user": 1,
                                     "successful_tool_result": True, "last_error": None,
                                     "reply_settings": external_b, "routing_state_settings": external_b}
            connected["done"] = True
        finally:
            client.close()
            gate.clients.remove(client)

    def verify(name, sample):
        if name == "live-start":
            if len(sample["state"]) != 2:
                return False
            handles[:] = [entry["overlay"]["hwnd"] for entry in sample["state"]]
        reports = gate.probes()
        if name == "live-legacy-click-A-down":
            if len(reports) != 2 or not (reports[0].get("buttons", 0) & 1):
                return False
            sample["target_probe_pid"] = gate.pids[0]
        if name == "live-legacy-click-A-up" and (len(reports) != 2 or reports[0].get("buttons") or reports[1].get("buttons")):
            return False
        if name == "live-legacy-click-B-down":
            if len(reports) != 2 or not (reports[1].get("buttons", 0) & 1):
                return False
            sample["target_probe_pid"] = gate.pids[1]
        if name == "live-legacy-click-B-up" and (len(reports) != 2 or reports[0].get("buttons") or reports[1].get("buttons")):
            return False
        if name in ("live-hold-B", "live-apply-A", "live-stage-B", "live-switch-back-B") and not held_f6():
            return False
        if name == "live-output":
            assert navigation_snapshot(gate)["selected_user"] == 1
        if name == "live-select-A":
            assert page()["selected_identity"] == gate.ids[0]
        if name == "live-kind-A":
            assert page()["drafts"][0][1]["buttons"][3] == {"type": "key", "key": 65}
        if name == "live-apply-A":
            ack_path = gate.run / "ui.output.acks.jsonl"
            acks = [json.loads(line) for line in ack_path.read_text(encoding="utf-8").splitlines()]
            if not any(ack.get("acknowledged") and ack.get("user") == 0 and not ack.get("error")
                       and settings_match(ack, 0, apply_a) for ack in acks):
                return False
            output = read_json(gate.run / "ui.output.json")
            assert settings_match(output, 0, apply_a)
            snapshot = page()
            assert snapshot["committed"][0] == [gate.ids[0], apply_a]
            assert snapshot["drafts"][0] == [gate.ids[0], apply_a]
            sample["gui_apply_ack"], sample["output_page"] = output, snapshot
        if name == "live-stage-B":
            snapshot = page()
            assert snapshot["selected_identity"] == gate.ids[1]
            assert snapshot["drafts"][1] == [gate.ids[1], draft_b]
            assert snapshot["committed"][1] == [gate.ids[1], legacy]
            sample["output_page"] = snapshot
        if name in ("live-switch-A", "live-switch-back-B"):
            snapshot = page()
            assert snapshot["selected_identity"] == gate.ids[0 if name == "live-switch-A" else 1]
            if name == "live-switch-back-B":
                assert snapshot["drafts"][1] == [gate.ids[1], draft_b]
            sample["output_page"] = snapshot
        if name == "live-agent-page":
            if not agent_path.exists():
                return False
            policy = read_json(agent_path)
            assert not policy["policy_enabled"] and not policy["capture"]
            assert not policy["session_control"] and not policy["app_preferences"]
            assert policy["selected_users"] == [] and policy["allowed_pids"] == []
            sample["agent_policy_before_grant"] = policy
        if name == "live-enable-agent":
            policy = read_json(agent_path)
            if not policy["policy_enabled"]:
                return False
            assert policy["selected_users"] == sorted(gate.ids)
            assert policy["allowed_pids"] == sorted(gate.pids)
            assert policy["session_control"] and not policy["capture"] and not policy["app_preferences"]
            acks = [json.loads(line) for line in (gate.run / "ui.agent.acks.jsonl").read_text().splitlines()]
            assert acks and acks[-1]["policy_enabled"] and acks[-1]["session_control"]
            sample["agent_policy_after_grant"] = policy
        if name == "live-connected-B-update":
            if not connected["done"]:
                connected_update()
                return False
            snapshot = page()
            profile_state = read_json(gate.run / "ui.profile.json")
            if snapshot["committed"][1] != [gate.ids[1], external_b]:
                return False
            if profile_state["profile"]["seats"][1]["output"] != external_b:
                return False
            assert snapshot["drafts"][1] == [gate.ids[1], draft_b]
            assert snapshot["committed"][0] == [gate.ids[0], apply_a]
            assert profile_state["profile"]["seats"][0]["output"] == apply_a
            assert profile_state["profile"]["seats"][1]["output"] == external_b
            assert held_f6(), "Connected output update disturbed the unrelated manual F6 hold on B"
            sample["connected_update"] = connected["summary"]
            sample["output_page"], sample["profile_state"] = snapshot, profile_state
        if name == "live-release-F6" and not released_f6():
            return False
        if name == "live-stop":
            recovery = read_json(gate.run / "ui.recovery.json")
            if recovery.get("phase") != "stopped":
                return False
            if recovery.get("error") is not None or recovery.get("targets") or not recovery.get("can_start"):
                return False
            if any(ui.user32.IsWindow(handle) for handle in handles):
                return False
            if any(report.get("held_keys") or report.get("buttons") for report in sample["probes"]):
                return False
            if list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{gate.gui.pid}-*.json")):
                return False
            sample["stopped_recovery"] = recovery
        return True

    def before_controls(current):
        return current.main_hwnd() is not None

    def after_controls(current):
        current.expected_exit = True
        hwnd = current.main_hwnd()
        assert hwnd and geometry.user32.PostMessageW(hwnd, 0x10, 0, 0)
        current.gui.wait(timeout=10)
        assert current.gui.returncode == 0
        return True

    try:
        gate.fixtures()
        for target in steps:
            if target["id"] == "live-enable-agent":
                target["fields"]["agent_pids"] = ",".join(map(str, gate.pids))
        profile = profile_for_fixtures()
        prefs = preferences()
        gate.evidence["scope"] = (
            "Two owned x64 automation probes; GUI Apply for A and successful connected routing_output configure for B; "
            "session-control grant limited to fixture users/PIDs; capture and app-preferences false"
        )
        gate.execute(profile, prefs, steps, verify, before_controls, after_controls)
        gate.evidence.update(result="PASS", live_fixture_architectures=["x64", "x64"],
                             connected_update_ack=connected["summary"],
                             output_ack_scope="GUI Apply for A plus successful connected configure for B")
        gate.save()
        return gate.run
    finally:
        finish_lifecycle_gate(gate, lifecycle)

def parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ready", action="store_true")
    parser.add_argument("--baseline-executable", type=Path)
    parser.add_argument("--phase", choices=("offline", "layout", "live", "all"), default="offline")
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--x86-executable", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--prior-offline", type=Path)
    return parser.parse_args()


def main():
    args = parse_args()
    assert args.ready, "Wait for root's frozen build and explicit exclusive-slot handoff"
    assert args.executable.is_file() and args.x86_executable.is_file()
    if args.baseline_executable is not None:
        assert args.baseline_executable.is_file()
    if args.baseline_executable is not None:
        baseline_phase(args, args.baseline_executable)
    if args.phase in ("offline", "all"):
        offline_phase(args, args.executable, "feature-workflows-ux-offline")
    if args.phase == "layout":
        assert args.prior_offline is not None, "Layout replacement evidence reuses one explicitly named 115-observation run"
        layout_phase(args, args.executable)
    if args.phase in ("live", "all"):
        live_phase(args, args.executable)
    print("Requested feature-workflow UX phase completed; inspect the retained evidence before accepting.", flush=True)


if __name__ == "__main__":
    main()
