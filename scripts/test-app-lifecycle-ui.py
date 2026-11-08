"""Real-control driver for the six-group lifecycle gate; preparation only.

Final workflow labels/fields are bound after consumer source freeze. No product
or native code is imported merely for AST checking. Existing Script receipts
drive real app-local egui controls; external actions use only our GUI HWND.
"""
import argparse
import copy
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
import json
import re
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import uuid

def module(name, file):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(file))
    result=importlib.util.module_from_spec(spec);spec.loader.exec_module(result)
    return result

def gate_type():
    maintenance=module("lifecycle_maintenance","test-profile-maintenance.py")
    wheel=module("lifecycle_fixture_wheel","test-horizontal-wheel-ui.py")
    geometry=module("lifecycle_geometry","owned-gui-geometry.py")
    tray=module("lifecycle_tray","owned-tray-menu.py")
    hotkeys=module("lifecycle_hotkeys","owned-app-hotkeys.py")
    ui=maintenance.ui
    geometry.user32.IsIconic.argtypes=[wintypes.HWND]
    geometry.user32.PostMessageW.argtypes=[wintypes.HWND,wintypes.UINT,ctypes.c_size_t,ctypes.c_ssize_t]
    geometry.user32.GetWindowTextW.argtypes=[wintypes.HWND,wintypes.LPWSTR,ctypes.c_int]

    class LifecycleGate(maintenance.ProfileGate):
        def __init__(self,args,label,offline=False,count=2):
            super().__init__(args,label,offline=offline,count=count)
            self.preferences_path=self.run/"owned preferences.json"
            self.startup_key="Software\\ProtoInputMultiPlex\\Tests\\"+str(uuid.uuid4())
            self.once_results={}
            self.expected_exit=False
            self.gui=None
            self.retained_main_hwnd=None

        def fixtures(self):
            self.args.assigned=False
            try:wheel.WheelUi.fixtures(self)
            except Exception:
                self.close()
                raise

        def once(self,name,operation):
            if name not in self.once_results:
                self.once_results[name]=operation()
                self.evidence.setdefault("external_owned_actions",{})[name]=self.once_results[name]
                self.save()
            return self.once_results[name]

        def lifecycle_hwnd(self):
            return hotkeys.lifecycle_window(self.gui.pid) if self.gui else None

        def main_hwnd(self):
            matches=[]
            @geometry.callback_type
            def visit(hwnd,unused):
                pid=wintypes.DWORD()
                geometry.user32.GetWindowThreadProcessId(hwnd,ctypes.byref(pid))
                if pid.value!=self.gui.pid or geometry.user32.GetWindow(hwnd,4):return True
                title=ctypes.create_unicode_buffer(64)
                geometry.user32.GetWindowTextW(hwnd,title,len(title))
                if title.value!="ProtoInputMultiPlex":return True
                bounds=geometry.Rect()
                # The exact own PID/title excludes the separately named
                # lifecycle helper. A genuine iconic main has a0x0 client;
                # cold Restore must still discover and retain that HWND.
                if geometry.user32.GetClientRect(hwnd,ctypes.byref(bounds)):
                    matches.append(int(hwnd))
                return True
            assert geometry.user32.EnumWindows(visit,0)
            assert len(matches)<=1,"Ambiguous own GUI main viewport"
            if matches:self.retained_main_hwnd=matches[0]
            return matches[0]if matches else None

        def retained_main_state(self):
            """Observe our exact known HWND even with a zero-size iconic client."""
            hwnd=self.retained_main_hwnd;assert hwnd
            pid=wintypes.DWORD();thread=geometry.user32.GetWindowThreadProcessId(hwnd,ctypes.byref(pid))
            rect=geometry.Rect();has_rect=bool(geometry.user32.GetClientRect(hwnd,ctypes.byref(rect)))
            observed={"hwnd":hwnd,"IsWindow":bool(ui.user32.IsWindow(hwnd)),
                "IsWindowVisible":bool(geometry.user32.IsWindowVisible(hwnd)),
                "IsIconic":bool(geometry.user32.IsIconic(hwnd)),"pid":pid.value,"thread":thread,
                "client_pixels":[rect.right-rect.left,rect.bottom-rect.top]if has_rect else None,"ui_files":{}}
            assert not observed["IsWindow"]or pid.value==self.gui.pid
            for name in("ui.progress.json","ui.ready.json"):
                path=self.run/name
                try:observed["ui_files"][name]={"mtime_ns":path.stat().st_mtime_ns,"value":ui.read_json(path)}
                except(OSError,ValueError)as error:observed["ui_files"][name]={"read_error":repr(error)}
            return observed

        def native_menu(self,label):
            hwnd=self.lifecycle_hwnd()
            assert hwnd,"Own lifecycle window has not appeared"
            opened=tray.open_menu(self.gui.pid,hwnd,0x8051,0x5049,0x0205)
            assert [tray.normalized_name(item["name"])for item in opened["items"]]==[
                "Open ProtoInputMultiPlex","Pause / Resume","Stop routing","Quit"]
            return tray.invoke_item(opened,label)

        def typed_hotkey(self,identifier):
            hwnd=self.lifecycle_hwnd()
            assert hwnd
            return hotkeys.typed_hotkey(self.gui.pid,hwnd,identifier)

        def icon(self):
            hwnd=self.lifecycle_hwnd()
            assert hwnd
            return tray.icon_rectangle(self.gui.pid,hwnd,0x5049)

        def app_snapshot(self,filename):
            # Final producer supplies the exact sanitized Script snapshot name.
            path=self.run/filename
            return ui.read_json(path) if path.exists() else None

        def screenshot(self,name):
            if not hasattr(self,"shot_started"):self.shot_started={}
            self.shot_started.setdefault(name,time.time())
            hwnd=geometry.owned_window(self.gui.pid)
            if not hwnd:return None
            return geometry.copy_owned_product_frame(self.run/"ui.png",self.run/(name+".png"),
                hwnd,self.shot_started[name])

        def execute(self,profile,preferences,steps,verify,before_controls=None,after_controls=None,launch_command=None):
            ui.put(self.run/"profile.json",profile)
            ui.put(self.run/"ui.json",steps)
            if launch_command is None:ui.put(self.preferences_path,preferences)
            else:assert ui.read_json(self.preferences_path)==preferences,"Recorded cold command must consume the existing accepted document"
            provenance=self.run/"runner-source";provenance.mkdir(exist_ok=True)
            scripts=Path(__file__).resolve().parent
            for name in("test-app-lifecycle-ui.py","test-app-lifecycle-acceptance.py","owned-app-hotkeys.py","owned-tray-menu.py"):
                shutil.copy2(scripts/name,provenance/name)
            ui.put(provenance/"dependency-hashes.json",{p.name:hashlib.sha256(p.read_bytes()).hexdigest()for p in scripts.glob("*.py")})
            shutil.copy2(self.run/"ui.json",provenance/"ui.json")
            shutil.copy2(self.preferences_path,provenance/"initial-preferences.json")
            command=[str(self.images[0]),"gui","--profile",str(self.run/"profile.json"),
                "--preferences",str(self.preferences_path),"--startup-test-key",self.startup_key,
                "--ui-script",str(self.run/"ui.json")]
            if launch_command is not None:
                command=launch_command+f' --startup-test-key "{self.startup_key}" --ui-script "{self.run/"ui.json"}"'
            self.gui=subprocess.Popen(command,cwd=self.run if self.offline else Path(__file__).resolve().parent.parent,
                creationflags=subprocess.CREATE_NO_WINDOW)
            self.owned.append(self.gui)
            self.evidence["gui_pid"]=self.gui.pid
            self.evidence["startup_test_key"]=self.startup_key
            deadline=time.monotonic()+360;count=0;changed=time.monotonic();prepared=False
            observation_timeout=getattr(self.args,"review_timeout",45)
            try:
                while time.monotonic()<deadline:
                    if not self.expected_exit:assert self.gui.poll() is None,"Own GUI exited before acknowledged cleanup"
                    if not prepared:
                        prepared=before_controls is None or before_controls(self)
                        if not prepared:
                            assert time.monotonic()-changed<observation_timeout,"Initial own window/lifecycle preparation did not complete"
                            time.sleep(.05);continue
                        changed=time.monotonic()
                    path=self.run/"ui.events.jsonl"
                    try:events=[json.loads(line)for line in path.read_text(encoding="utf-8").splitlines()]
                    except (OSError,ValueError):time.sleep(.05);continue
                    self.assert_event_contract(steps,events)
                    if len(events)!=count:count=len(events);changed=time.monotonic()
                    assert time.monotonic()-changed<observation_timeout,f"No accepted lifecycle control/sample after {count}/{len(steps)}"
                    if count and steps[count-1]["id"] not in self.evidence["samples"]:
                        if time.monotonic()-changed<.35:time.sleep(.05);continue
                        try:
                            ready = ui.read_json(self.run/"ui.ready.json")
                        except (OSError,ValueError):
                            # The opt-in Script status files are replaced each
                            # frame; an in-flight empty write is not a product
                            # readiness failure. Retain the finite deadline.
                            time.sleep(.05);continue
                        if not ready:
                            time.sleep(.05);continue
                        target=steps[count-1]
                        try:
                            sample={"event":events[-1],"progress":ui.read_json(self.run/"ui.progress.json"),
                                "probes":self.probes(),"profile_state":self.snapshot(),
                                "state":ui.read_json(self.run/"ui.state.json") if (self.run/"ui.state.json").exists() else []}
                            accepted=verify(target["id"],sample)
                        except (OSError,ValueError):time.sleep(.05);continue
                        if not accepted:time.sleep(.05);continue
                        self.evidence["samples"][target["id"]]=sample
                        self.evidence["clicked_events"]=events
                        self.save()
                        (self.run/"ui.sampled.json").write_text(str(count),encoding="utf-8")
                        changed=time.monotonic()
                        print(f"Lifecycle UI accepted {target['id']}: {target['button']}",flush=True)
                    if count==len(steps) and all(s["id"]in self.evidence["samples"]for s in steps):
                        if after_controls is not None and not after_controls(self):time.sleep(.05);continue
                        self.evidence["result"]="PASS"
                        print(f"PASS: {len(steps)} real control observations. Evidence: {self.run}",flush=True)
                        return
                    time.sleep(.05)
                raise AssertionError("Finite lifecycle GUI deadline exhausted")
            except Exception as error:
                self.evidence["failure"]=repr(error)
                print(f"NOT ACCEPTED: {error!a}. Evidence: {self.run}",flush=True)
                raise
            finally:self.save()

    return LifecycleGate,ui,hotkeys,geometry

def preference_defaults():
    return {"version":1,"appearance_mode":"dark","zoom_percent":100,
        "launch_minimized":True,"minimize_to_tray":True,"close_to_tray":True,
        "launch_at_login":False,"shortcuts":[{"action":"stop","key":"f11",
        "modifiers":{"control":True,"alt":True,"shift":True,"windows":False}}]}

def startup_value(key):
    """Read ONLY our disposable test key; never fall back to the real Run key."""
    import winreg
    assert key.startswith("Software\\ProtoInputMultiPlex\\Tests\\") and len(key.split("\\")[-1])==36
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER,key,0,winreg.KEY_READ)as opened:
            try:value,kind=winreg.QueryValueEx(opened,"ProtoInputMultiPlex")
            except FileNotFoundError:return None
            return {"kind":kind,"value":value}
    except FileNotFoundError:return None

def validate_startup_command(command,executable,preferences):
    matched=re.fullmatch(r'"([^\"]+)" gui --preferences "([^\"]+)"',command)
    assert matched,"Startup command must quote exactly the executable/preferences and contain only the GUI command"
    assert Path(matched[1]).samefile(executable)and Path(matched[2]).samefile(preferences),"Startup command references a different file"
    return command

def startup_raw(key):
    """Exact type/bytes of one owned value, never a production registry key."""
    import winreg
    assert key.startswith("Software\\ProtoInputMultiPlex\\Tests\\")and len(key.split("\\")[-1])==36
    advapi=ctypes.WinDLL("advapi32")
    advapi.RegQueryValueExW.argtypes=[wintypes.HKEY,wintypes.LPCWSTR,ctypes.c_void_p,
        ctypes.POINTER(wintypes.DWORD),ctypes.c_void_p,ctypes.POINTER(wintypes.DWORD)]
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER,key,0,winreg.KEY_QUERY_VALUE)as opened:
        kind=wintypes.DWORD();size=wintypes.DWORD()
        assert advapi.RegQueryValueExW(int(opened),"ProtoInputMultiPlex",None,ctypes.byref(kind),None,ctypes.byref(size))==0
        assert size.value<=32768
        value=ctypes.create_string_buffer(size.value)
        assert advapi.RegQueryValueExW(int(opened),"ProtoInputMultiPlex",None,ctypes.byref(kind),value,ctypes.byref(size))==0
        return kind.value,value.raw[:size.value]

def remove_startup_key(key):
    import winreg
    assert key.startswith("Software\\ProtoInputMultiPlex\\Tests\\") and len(key.split("\\")[-1])==36
    try:winreg.DeleteKey(winreg.HKEY_CURRENT_USER,key)
    except FileNotFoundError:pass

def main_workflow(args):
    Gate,ui,hotkeys,geometry=gate_type()
    connected=module("lifecycle_connected","test-connected-agents-acceptance.py")
    gate=Gate(args,"app-lifecycle-main",count=2)
    conflict=None;broker=None;descriptor=None;accepted=None;canonical=None;overlays=[];fresh_overlays=[]
    preferences=preference_defaults();startup_command=None;mode_done=False
    if args.start_at:
        previous=ui.read_json(args.prefix/"evidence.json")
        required={"minimize":"home-close-hide","arm-session-only":"minimize","apply-app-grant":"grant-app"}.get(args.start_at,"apply-shortcuts")
        assert previous["result"]=="NOT ACCEPTED"and required in previous["samples"]
        preferences=ui.read_json(args.prefix/"owned preferences.json")
        # Materialize only the new version1 defaults in this fresh fixture;
        # preserve the historical prefix file and its original provenance.
        preferences.setdefault("appearance_mode","dark")
        preferences.setdefault("zoom_percent",100)
        preferences["launch_at_login"]=False
        if args.start_at in("home-profile","minimize","arm-session-only","apply-app-grant"):preferences["launch_minimized"]=False
        accepted=copy.deepcopy(preferences)
        expected_order=["stop","pause_resume","toggle_routing","switched","concurrent","toggle_mode","toggle_keyboard"]
        assert [s["action"]for s in accepted["shortcuts"]]==expected_order
        gate.evidence["fresh_seeded_hotkey_ids"]={action:0x5100+i for i,action in enumerate(expected_order)}
    step=ui.step
    steps=[step("settings","Settings"),step("preferences","Application preferences"),
        step("enable-pause","Shortcut Pause or resume"),
        step("invalid-shortcut","Apply application preferences",{"app_shortcut_PauseResume":"Ctrl+A+B"},disabled=True),
        step("correct-shortcut","Apply application preferences",{"app_shortcut_PauseResume":"Ctrl+Alt+Shift+F6"}),
        step("revert-shortcut","Revert application preferences"),
        step("enable-routing","Shortcut Toggle routing",{"app_shortcut_ToggleRouting":"Ctrl+Alt+Shift+F7"}),
        step("enable-switched","Shortcut Use switched mode",{"app_shortcut_Switched":"Ctrl+Alt+Shift+F4"}),
        step("enable-concurrent","Shortcut Use concurrent mode",{"app_shortcut_Concurrent":"Ctrl+Alt+Shift+F3"}),
        step("enable-mode","Shortcut Toggle switched/concurrent",{"app_shortcut_ToggleMode":"Ctrl+Alt+Shift+F8"}),
        step("enable-keyboard","Shortcut Toggle multi-keyboard",{"app_shortcut_ToggleKeyboard":"Ctrl+Alt+Shift+F2"}),
        step("apply-shortcuts","Apply application preferences"),
        step("startup-on","Launch at login (local only)"),step("apply-startup-on","Apply application preferences"),
        step("startup-off","Launch at login (local only)"),step("apply-startup-off","Apply application preferences"),
        step("home-profile","Back to Home"),step("settings-profile","Settings"),step("profiles","Profiles & backend"),
        step("import-b","Import profile",{"profile_transfer_path":gate.run/"profile-B.json"}),
        step("reset-profile","Reset profile"),step("undo-profile","Undo profile change"),
        step("import-a","Import profile",{"profile_transfer_path":gate.run/"profile-A.json"}),step("home-start","Back to Home"),
        step("start","Start routing"),step("settings-lab","Settings"),step("lab","Input lab"),
        step("focus-a","1 Hold click"),step("focus-a-up","1 Release click"),
        step("focus-b","2 Hold click"),step("focus-b-up","2 Release click"),
        step("hold-a","Hold key",{"selected_user":0,"key_name":"f5"}),
        step("hold-b","Hold key",{"selected_user":1,"key_name":"f6"}),
        step("home-conflict","Back to Home"),step("settings-conflict","Settings"),step("preferences-conflict","Application preferences"),
        step("conflicting-apply","Apply application preferences",{"app_shortcut_Stop":"Ctrl+Alt+Shift+F10","app_shortcut_PauseResume":"Ctrl+Alt+Shift+F9"}),
        step("revert-conflict","Revert application preferences"),step("preferences-image","Revert application preferences"),
        step("home-close-hide","Back to Home"),step("minimize","Hide"),
        step("settings-agent","Settings"),step("agent","Agent Access"),
        step("arm-session-only","Enable agent access"),step("grant-app","Agent app-preferences permission"),
        step("apply-app-grant","Apply access policy"),step("home-rehold","Back to Home"),
        step("settings-rehold","Settings"),step("lab-rehold","Input lab"),
        step("rehold-a","Hold key",{"selected_user":0,"key_name":"f5"}),
        step("rehold-b","Hold key",{"selected_user":1,"key_name":"f6"}),
        step("home-tray-stop","Back to Home"),step("settings-fresh","Settings"),step("profiles-fresh","Profiles & backend"),
        step("load-fresh","Load profile",{"profile_path":gate.run/"fresh-profile.json"}),
        step("home-toggle-start","Back to Home"),step("settings-fresh-lab","Settings"),step("fresh-lab","Input lab"),
        step("fresh-focus","1 Hold click"),step("fresh-focus-up","1 Release click"),
        step("fresh-hold","Hold key",{"selected_user":0,"key_name":"f5"}),step("home-quit","Back to Home")]
    steps[0]["wait_ms"]=12000
    if args.start_at:
        offset=next(i for i,s in enumerate(steps)if s["id"]==args.start_at)
        if args.start_at in("arm-session-only","apply-app-grant"):
            # Match the retained post-Pause precondition: targets focused, keys
            # released. Their first native key pair will be rehold -> Stop.
            setup_ids=["start","settings-lab","lab","focus-a","focus-a-up","focus-b","focus-b-up","home-conflict","settings-agent","agent"]
            if args.start_at=="apply-app-grant":setup_ids += ["arm-session-only","grant-app"]
        elif args.start_at=="minimize":
            setup_ids=["start","settings-lab","lab","focus-a","focus-a-up","focus-b","focus-b-up","hold-a","hold-b","home-conflict"]
        else:setup_ids=["settings","preferences"]
        steps=[next(s for s in steps if s["id"]==name)for name in setup_ids]+steps[offset:]
        steps[0]["wait_ms"]=12000
        gate.evidence["retained_accepted_prefix"]={"path":str(args.prefix/"evidence.json"),"accepted_samples":list(previous["samples"]),
            "setup_repeated_in_this_tail":setup_ids,"seeded_accepted_preferences":preferences,
            "original_shortcut_prefix":previous.get("retained_accepted_prefix")}
    if args.main_tail:
        prior=ui.read_json(args.prefix/"evidence.json")
        assert prior["result"]=="NOT ACCEPTED"and"invalid-shortcut"in prior["samples"]
        steps=[s for s in steps if s["id"]!="invalid-shortcut"]
        next(s for s in steps if s["id"]=="enable-pause")["fields"]={"app_shortcut_PauseResume":"Ctrl+A+B"}
        gate.evidence["retained_accepted_prefix"]={"path":str(args.prefix/"evidence.json"),"accepted_samples":list(prior["samples"]),
            "setup_repeated_in_this_tail":["settings","preferences","enable-pause"]}
    def prefs():return ui.read_json(gate.preferences_path)
    def app():return gate.app_snapshot("ui.app-preferences.json")
    def key_edges(reports,index):return [(e["vk"],e["pressed"])for e in reports[index]["key_events"]]
    def original_holds():
        reports=gate.probes()
        assert 116 in reports[0]["held_keys"] and 117 in reports[1]["held_keys"],"Preference/window action released unrelated ordinary holds"
    def visible():
        hwnd=gate.main_hwnd()
        return hwnd and bool(geometry.user32.IsWindowVisible(hwnd))and not geometry.user32.IsIconic(hwnd)
    def restore(name):
        before=gate.retained_main_state()
        result=gate.native_menu("Open ProtoInputMultiPlex")
        record={"actual_MSAA_result":result,"before":before,"post_dispatch":gate.retained_main_state()}
        gate.evidence.setdefault("external_owned_actions",{})[f"menu-Restore-{name}-dispatch"]=record
        gate.save()
        try:
            record["restored"]=ui.eventually(gate.retained_main_state,lambda value:
                value["IsWindow"]and value["IsWindowVisible"]and not value["IsIconic"]
                and value["client_pixels"]and min(value["client_pixels"])>0)
        except AssertionError as error:
            record["timeout_state"]=gate.retained_main_state();record["failure"]=repr(error);gate.save();raise
        gate.save()
        return result
    def before_controls(g):
        if args.start_at in("home-profile","minimize","arm-session-only","apply-app-grant"):
            if not visible()or not g.lifecycle_hwnd():return False
            assert not(g.run/"ui.state.json").exists()
            g.evidence["continuation_scope"]="Visible-start seeded accepted shortcuts; startup command criteria belong to separate bounded tails"
            return True
        if not g.lifecycle_hwnd()or not g.main_hwnd():return False
        hwnd=g.main_hwnd()
        if geometry.user32.IsWindowVisible(hwnd)and not geometry.user32.IsIconic(hwnd):return False
        assert not (g.run/"ui.state.json").exists(),"Cold launch created routing state"
        assert not list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{g.gui.pid}-*.json"))
        assert not startup_value(g.startup_key)
        g.evidence["cold_launch"]={"minimized_or_hidden":True,"no_routing_state_or_descriptor":True,"tray_icon":g.icon()}
        assert g.evidence["cold_launch"]["tray_icon"]["hresult"]==0
        g.once("cold-actual-menu-restore",lambda:restore("cold"))
        return True
    def verify(name,sample):
        nonlocal conflict,broker,descriptor,accepted,canonical,overlays,fresh_overlays,startup_command,mode_done
        current=sample["profile_state"]["profile"];reports=sample["probes"]
        if canonical is None:canonical=copy.deepcopy(current)
        if name=="preferences":
            assert app()["committed"]==preferences and prefs()==preferences
        if name=="invalid-shortcut":assert prefs()==preferences
        if name=="correct-shortcut":
            if not any(s["action"]=="pause_resume"and s["key"]=="f6"for s in prefs()["shortcuts"]):
                assert "not applied"not in sample["profile_state"]["status"].lower(),sample["profile_state"]["status"]
                return False
        if name=="apply-shortcuts":
            if len(prefs()["shortcuts"])!=7:
                assert "not applied"not in sample["profile_state"]["status"].lower(),sample["profile_state"]["status"]
                return False
            accepted=copy.deepcopy(prefs())
            assert {s["action"]for s in accepted["shortcuts"]}=={"stop","pause_resume","toggle_routing","switched","concurrent","toggle_mode","toggle_keyboard"}
            assert app()["committed"]==accepted
            def stopped_mode_guard():
                sent=gate.typed_hotkey(0x5105)
                rejected=ui.eventually(gate.snapshot,lambda value:"session"in value["status"].lower())
                assert rejected["profile"]==canonical and not(gate.run/"ui.state.json").exists()
                return {"typed_mode":sent,"visible_rejection":rejected["status"],"complete_profile_unchanged":True}
            sample["stopped_mode_guard"]=gate.once("stopped-automatic-mode-rejected",stopped_mode_guard)
        if name=="apply-startup-on":
            if not prefs()["launch_at_login"]:return False
            actual=startup_value(gate.startup_key)
            assert actual and actual["kind"]==1,actual
            startup_command=validate_startup_command(actual["value"],gate.images[0],gate.preferences_path)
            assert prefs()["launch_at_login"]and current==canonical
            sample["actual_disposable_startup_value"]=actual
        if name=="apply-startup-off":
            if prefs()["launch_at_login"]:return False
            assert startup_value(gate.startup_key)is None and prefs()==accepted
        if name in ("import-b","undo-profile"):
            assert current["seats"][0]["name"]=="Owned alternate lifecycle"
            if name=="import-b":
                def fixed_mode_guard():
                    before=copy.deepcopy(current)
                    sent=gate.typed_hotkey(0x5105)
                    rejected=ui.eventually(gate.snapshot,lambda value:"automatic"in value["status"].lower())
                    assert rejected["profile"]==before and rejected["profile"]["desktop"]is None
                    return {"typed_mode":sent,"visible_rejection":rejected["status"],"scope":"stopped fixed configuration"}
                sample["fixed_mode_guard"]=gate.once("stopped-fixed-mode-rejected",fixed_mode_guard)
        elif name=="reset-profile":assert not current["seats"]and prefs()==accepted
        elif name not in ("profiles","settings-profile","home-profile","settings","preferences") and name not in ("load-fresh","home-toggle-start","settings-fresh-lab","fresh-lab","fresh-focus","fresh-focus-up","fresh-hold","home-quit"):
            if name not in ("startup-on","apply-startup-on","startup-off"):assert current==canonical,"Unrelated complete profile changed"
        if name=="start":
            if len(sample["state"])!=2:return False
            overlays=[u["overlay"]["hwnd"]for u in sample["state"]]
        if name in ("hold-a","rehold-a") and 116 not in reports[0]["held_keys"]:return False
        if name in ("hold-b","rehold-b") and 117 not in reports[1]["held_keys"]:return False
        if name=="preferences-conflict":
            original_holds();conflict=hotkeys.OwnedHotkeys()
            result=conflict.register(0x78)
            assert result["accepted"],"Owned F9 conflict setup failed; do not disturb other registrations"
            sample["owned_conflict_registration"]=result
        if name=="conflicting-apply":
            if "not applied"not in sample["profile_state"]["status"].lower():return False
            original_holds();assert prefs()==accepted and app()["committed"]==accepted
            assert sample["profile_state"]["status"]and"not applied"in sample["profile_state"]["status"].lower()
            sample["rolled_back_staged_F10_probe"]=conflict.probe(0x79,True)
            sample["accepted_F11_still_reserved"]=conflict.probe(0x7A,False)
        if name=="revert-conflict":
            original_holds();assert prefs()==accepted
            conflict.close();conflict=None
        if name=="preferences-image":
            original_holds()
            shot=gate.screenshot("application-preferences")
            if not shot:return False
            sample["owned_preferences_image"]=shot
        if name=="home-close-hide":
            original_holds()
            def close_restore():
                hwnd=gate.main_hwnd();assert hwnd
                assert geometry.user32.PostMessageW(hwnd,0x10,0,0)
                ui.eventually(lambda:geometry.user32.IsWindowVisible(hwnd),lambda value:not value)
                original_holds()
                return restore("close-to-tray")
            gate.once("actual-WM_CLOSE-hide-and-menu-restore",close_restore)
            original_holds()
        if name=="minimize":
            def minimized_actions():
                ui.eventually(visible,lambda value:not value)
                original_holds();restore("minimize")
                paused=gate.native_menu("Pause / Resume")
                ui.eventually(gate.probes,lambda values:all(vk not in values[i]["held_keys"]for i,vk in enumerate((116,117))))
                assert key_edges(gate.probes(),0)==[(116,True),(116,False)]and key_edges(gate.probes(),1)==[(117,True),(117,False)]
                resumed=gate.native_menu("Pause / Resume")
                ui.eventually(lambda:gate.snapshot()["status"],lambda text:"resume"in text.lower())
                def change(identifier,mode,keyboards):
                    sent=gate.typed_hotkey(identifier)
                    ui.eventually(gate.snapshot,lambda value:value["profile"]["desktop"]["mode"]==mode and value["profile"]["desktop"]["multi_keyboard"]is keyboards)
                    ui.eventually(lambda:ui.read_json(gate.run/"ui.ready.json"),bool)
                    return sent
                # IDs follow the exact accepted registration order: initial
                # Stop; then Pause; then one candidate ordered by UI actions.
                typed=[change(0x5105,"switched",True),change(0x5104,"concurrent",True),
                    change(0x5103,"switched",True),change(0x5106,"switched",False),
                    change(0x5106,"switched",True),change(0x5105,"concurrent",True)]
                return {"actual_menu_pause":paused,"actual_menu_resume":resumed,"typed_mode_keyboard_actions":typed}
            gate.once("minimize-restore-pause-resume-typed-actions",minimized_actions)
            mode_done=True
        if name=="arm-session-only":
            policy=ui.read_json(gate.run/"ui.agent.json")
            if not policy["policy_enabled"]or policy["pending"]:return False
            assert policy["session_control"]and not policy["app_preferences"]
            paths=list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{gate.gui.pid}-*.json"));assert len(paths)==1
            descriptor=paths[0]
            if args.start_at=="apply-app-grant":
                sample["setup_only"]="Policy arm before granting; prior193017denial/Inspect evidence retained, not replayed."
                return True
            broker=connected.ConnectedMcp(gate.images[0],descriptor,gate.run/"connected.stderr.log")
            gate.connections.append(broker)
            inspected=broker.tool("app_preferences",{"operation":"inspect"})
            assert inspected["source"]=="connected_gui"and inspected["preferences"]==accepted
            settings={k:v for k,v in accepted.items()if k not in("version","launch_at_login")}
            broker.tool("app_preferences",{"operation":"configure","settings":settings},error=True)
            assert prefs()==accepted and startup_value(gate.startup_key)is None
            sample["separate_app_permission_denied_with_session_grant"]=True
        if name=="apply-app-grant":
            policy=ui.read_json(gate.run/"ui.agent.json")
            if policy["pending"]or not policy["app_preferences"]:return False
            if broker is not None:
                broker.close();gate.connections.remove(broker);broker=None
            broker=connected.ConnectedMcp(gate.images[0],descriptor,gate.run/"connected-current-policy.stderr.log")
            gate.connections.append(broker)
            registration=ui.eventually(lambda:ui.read_json(gate.run/"ui.agent.json"),lambda value:
                value["policy_enabled"]and not value["pending"]and value["app_preferences"]and bool(value["clients"]))
            sample["fresh_current_policy_clients"]=registration["clients"]
            assert len(registration["clients"])==1,"Only our newly connected client should be present"
            current_client=registration["clients"][0]
            # Inspect confirms the accepted GUI preferences after reconnect;
            # it is one real app-preference acknowledgement, not a fake count.
            inspected=broker.tool("app_preferences",{"operation":"inspect"})
            assert inspected["source"]=="connected_gui"and inspected["preferences"]==accepted
            before=gate.probes();settings={k:v for k,v in accepted.items()if k not in("version","launch_at_login")}
            settings=copy.deepcopy(settings);settings["close_to_tray"]=False
            settings["appearance_mode"]="light"
            settings.pop("zoom_percent")  # Old-shape omission must retain100.
            applied=broker.tool("app_preferences",{"operation":"configure","settings":settings})
            assert applied["source"]=="connected_gui"and not applied["preferences"]["close_to_tray"]
            assert applied["preferences"]["appearance_mode"]=="light"and applied["preferences"]["zoom_percent"]==100
            assert prefs()==applied["preferences"]
            settings["close_to_tray"]=True
            settings["appearance_mode"]=accepted["appearance_mode"]
            restored=broker.tool("app_preferences",{"operation":"configure","settings":settings})
            assert restored["preferences"]==accepted and prefs()==accepted
            forbidden={**settings,"launch_at_login":True}
            broker.tool("app_preferences",{"operation":"configure","settings":forbidden},error=True)
            assert startup_value(gate.startup_key)is None
            after=gate.probes()
            assert [(p["key_events"],p["cursor"])for p in before]==[(p["key_events"],p["cursor"])for p in after]
            acks=[json.loads(line)for line in (gate.run/"ui.app-preferences.acks.jsonl").read_text(encoding="utf-8").splitlines()]
            expected_acks=3 if args.start_at=="apply-app-grant"else 4
            assert len(acks)==expected_acks and all(a["acknowledged"]and a["client"]>0 and a["sequence"]>0 for a in acks)
            current_acks=[a for a in acks if a["client"]==current_client]
            assert len(current_acks)==3
            assert len({a["sequence"]for a in current_acks})==3
            sample["tagged_app_acknowledgements"]=[{"client":a["client"],"sequence":a["sequence"],"error":a["error"]}for a in acks]
            broker.close();gate.connections.remove(broker);broker=None
            assert gate.gui.poll()is None
        if name=="home-tray-stop":
            original_holds()
            def stop_and_prepare_fresh():
                pairs=1 if args.start_at in("arm-session-only","apply-app-grant")else 2
                before=gate.probes()
                result=gate.native_menu("Stop routing")
                gate.evidence.setdefault("external_owned_actions",{})["actual-tray-Stop-dispatch"]={
                    "actual_MSAA_result":result,"main_window":gate.retained_main_state(),
                    "targets_before":[{k:p.get(k)for k in("pid","hwnd","capture_hwnd","held_keys","buttons","key_events")}for p in before],
                    "expected_key_pairs":pairs}
                gate.save()
                ui.eventually(lambda:all(not ui.user32.IsWindow(h)for h in overlays),bool)
                ui.eventually(gate.probes,lambda values:all(len(v["key_events"])>=2*pairs for v in values))
                assert key_edges(gate.probes(),0)==[(116,True),(116,False)]*pairs
                assert key_edges(gate.probes(),1)==[(117,True),(117,False)]*pairs
                assert not descriptor.exists()
                with hotkeys.OwnedHotkeys()as fixture:retained=fixture.probe(0x7A,False)
                # Fresh owned processes, never reattach the already-injected pair.
                gate.fresh_processes=[]
                for index in range(2):
                    process=subprocess.Popen([str(gate.images[index]),"probe","--automation","--report",str(gate.run/f"fresh-{index}.json")],creationflags=subprocess.CREATE_NO_WINDOW)
                    gate.owned.append(process);gate.fresh_processes.append(process)
                probe_read=lambda:[ui.read_json(gate.run/f"fresh-{i}.json")for i in range(2)]
                ui.eventually(probe_read,lambda values:all(v["frames"]>2 for v in values))
                pid_list=[p.pid for p in gate.fresh_processes]
                client=ui.Client(gate.images[0],args.runtime.resolve(),gate.run/"fresh-discovery.stderr.log")
                try:
                    inventory=ui.eventually(lambda:client.tool("routing_windows"),lambda value:all(any(w["pid"]==pid for w in value["windows"])for pid in pid_list))
                    wins=[next(w for w in inventory["windows"]if w["pid"]==pid)for pid in pid_list]
                    for i,w in enumerate(wins):assert ui.user32.SetWindowPos(w["hwnd"],wintypes.HWND(-1),30+i*460,430,420,360,0x10)
                    inventory=client.tool("routing_windows")
                    wins=[next(w for w in inventory["windows"]if w["pid"]==pid)for pid in pid_list]
                    points=[[w["client"]["left"]+120,w["client"]["top"]+100]for w in wins]
                finally:client.close()
                fresh=copy.deepcopy(canonical);fresh["desktop"]["allowed_pids"]=pid_list
                for i,s in enumerate(fresh["seats"]):s["desktop_start"]=points[i]
                ui.put(gate.run/"fresh-profile.json",fresh)
                gate.fresh_profile=fresh;gate.fresh_probes=probe_read
                return {"menu":result,"plain_native_UP":True,"native_key_pairs_per_original_target":pairs,
                    "broker_descriptor_removed":True,"F11_retained":retained,"fresh_pids":pid_list}
            gate.once("actual-tray-stop-fresh-target-preparation",stop_and_prepare_fresh)
        if name=="load-fresh":assert current==gate.fresh_profile
        if name=="home-toggle-start":
            def toggle_start():
                action=gate.typed_hotkey(0x5102)
                states=ui.eventually(lambda:ui.read_json(gate.run/"ui.state.json"),lambda values:len(values)==2 and all(ui.user32.IsWindow(u["overlay"]["hwnd"])for u in values))
                return {"typed_toggle":action,"overlays":[u["overlay"]["hwnd"]for u in states]}
            result=gate.once("typed-toggle-fresh-Start",toggle_start);fresh_overlays=result["overlays"]
        if name=="fresh-hold" and 116 not in gate.fresh_probes()[0]["held_keys"]:return False
        if name=="home-quit":
            assert 116 in gate.fresh_probes()[0]["held_keys"]
            # Disable close-to-tray through the explicitly granted connected tool
            # was restored above; leave close opt-out to the later cold command.
            return True
        return True
    def after_controls(g):
        def quit_actual():
            g.expected_exit=True
            action=g.native_menu("Quit")
            g.gui.wait(timeout=15)
            assert g.gui.returncode==0
            reports=ui.eventually(g.fresh_probes,lambda values:len(values[0]["key_events"])>=2)
            assert [(e["vk"],e["pressed"])for e in reports[0]["key_events"]]==[(116,True),(116,False)]
            assert all(not ui.user32.IsWindow(h)for h in fresh_overlays)
            assert not hotkeys.lifecycle_window(g.gui.pid)
            assert not list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{g.gui.pid}-*.json"))
            with hotkeys.OwnedHotkeys()as fixture:released=fixture.probe(0x7A,True)
            return {"menu":action,"plain_native_UP":True,"overlay_lifecycle_broker_destroyed":True,"F11_freed":released}
        g.once("actual-Quit-with-held-key",quit_actual)
        return True
    try:
        gate.fixtures()
        profile={"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":gate.pids},
            "seats":[{"id":f"lifecycle-own-{i}","name":f"Owned lifecycle {i+1}","mouse":f"virtual://mouse/{i+1}",
                "keyboard":f"virtual://keyboard/{i+1}","desktop_start":gate.points[i],"cursor":{"shape":"ring"if i==0 else"arrow","badge":"OWN"}}for i in range(2)]}
        ui.put(gate.run/"profile-A.json",profile)
        alternate=copy.deepcopy(profile);alternate["seats"][0]["name"]="Owned alternate lifecycle"
        alternate["desktop"]=None
        for i,seat in enumerate(alternate["seats"]):seat["target_pid"]=gate.pids[i]
        ui.put(gate.run/"profile-B.json",alternate)
        arm=next(s for s in steps if s["id"]=="arm-session-only")
        arm["fields"]={"agent_users":",".join(s["id"]for s in profile["seats"]),"agent_pids":",".join(map(str,gate.pids)),
            "agent_session_control":"true","agent_app_preferences":"false"}
        gate.execute(profile,preferences,steps,verify,before_controls,after_controls)
        gate.evidence["startup_command"]=startup_command
        gate.evidence["mode_dispatch_covered"]=mode_done
        gate.save()
        return gate.run
    finally:
        try:
            if conflict:conflict.close()
        finally:
            try:gate.close()
            finally:
                leftover=startup_value(gate.startup_key)
                if leftover:
                    gate.evidence["runner_removed_owned_startup_value_after_failure"]=leftover
                    gate.evidence["result"]="NOT ACCEPTED"
                remove_startup_key(gate.startup_key)
                gate.evidence["owned_startup_key_removed"]=startup_value(gate.startup_key)is None
                gate.save()

def cold_command(args):
    Gate,ui,hotkeys,geometry=gate_type()
    original=ui.read_json(args.from_main/"evidence.json")
    assert original["result"]=="PASS"
    command=original["startup_command"]
    main_image=args.from_main/"control64.exe"
    old_preferences=Path(original.get("startup_preferences_path",str(args.from_main/"owned preferences.json")))
    validate_startup_command(command,main_image,old_preferences)
    gate=Gate(args,"app-lifecycle-cold-command",offline=True,count=0)
    assert hashlib.sha256(main_image.read_bytes()).digest()==hashlib.sha256(gate.images[0].read_bytes()).digest()
    gate.preferences_path=old_preferences
    initial=ui.read_json(old_preferences)
    assert initial["launch_minimized"]and initial["close_to_tray"]and not initial["launch_at_login"]
    profile=ui.read_json(args.from_main/("profile-A.json"if(args.from_main/"profile-A.json").exists()else"profile.json"))
    steps=[ui.step("settings","Settings"),ui.step("preferences","Application preferences"),
        ui.step("opt-out-close","Close to tray"),ui.step("apply-close-optout","Apply application preferences"),
        ui.step("home-close","Back to Home")]
    steps[0]["wait_ms"]=12000
    def before(g):
        hwnd=g.main_hwnd()
        if not hwnd or not g.lifecycle_hwnd():return False
        if geometry.user32.IsWindowVisible(hwnd)and not geometry.user32.IsIconic(hwnd):return False
        assert not (g.run/"ui.state.json").exists()
        assert not list(Path(tempfile.gettempdir()).glob(f"protoinputmultiplex-agent-{g.gui.pid}-*.json"))
        action=g.native_menu("Open ProtoInputMultiPlex")
        ui.eventually(lambda:geometry.user32.IsWindowVisible(hwnd)and not geometry.user32.IsIconic(hwnd),bool)
        g.evidence["cold_exact_recorded_command"]={"command":command,"test_only_appended_arguments":["--startup-test-key","--ui-script"],
            "no_session_or_attachment":True,"actual_menu_restore":action}
        return True
    def verify(name,sample):
        assert not (gate.run/"ui.state.json").exists(),"Cold startup/profile silently started routing"
        assert not sample["profile_state"]["profile"]["seats"],"Exact startup command unexpectedly loaded or constructed a seat profile"
        if name=="apply-close-optout":
            expected=copy.deepcopy(initial);expected["close_to_tray"]=False
            assert ui.read_json(old_preferences)==expected
        if name=="home-close":
            shot=gate.screenshot("cold-empty-home")
            if not shot:return False
            sample["owned_default_empty_home_image"]=shot
        return True
    def after(g):
        hwnd=g.main_hwnd();assert hwnd
        g.expected_exit=True
        assert geometry.user32.PostMessageW(hwnd,0x10,0,0)
        g.gui.wait(timeout=10);assert g.gui.returncode==0
        assert not hotkeys.lifecycle_window(g.gui.pid)
        with hotkeys.OwnedHotkeys()as fixture:probe=fixture.probe(0x7A,True)
        g.evidence["normal_Close_optout"]={"method":"owned_WM_CLOSE","actual_process_exit":True,"F11_freed":probe}
        return True
    try:
        gate.execute(profile,initial,steps,verify,before,after,launch_command=command)
        return gate.run
    finally:gate.close();remove_startup_key(gate.startup_key)

def startup_tail(args):
    """One invalid provider boundary, then one valid short command workflow."""
    Gate,ui,hotkeys,geometry=gate_type()
    previous=ui.read_json(args.prefix/"evidence.json")
    assert previous["result"]=="NOT ACCEPTED"and"apply-shortcuts"in previous["samples"]
    seed=ui.read_json(args.prefix/"owned preferences.json")
    seed.setdefault("appearance_mode","dark")
    seed.setdefault("zoom_percent",100)
    seed["launch_at_login"]=False
    long_case=args.phase=="startup-long"
    gate=Gate(args,"app-lifecycle-startup-long"if long_case else"al",offline=True,count=0)
    initial=copy.deepcopy(seed)
    initial.update(launch_minimized=not long_case,minimize_to_tray=not long_case,close_to_tray=not long_case)
    profile={"version":1,"concurrent":True,"seats":[{"id":f"lifecycle-startup-own-{i}","name":f"Owned startup {i+1}",
        "mouse":f"virtual://mouse/{i+1}","keyboard":f"virtual://keyboard/{i+1}","target_pid":987001+i}for i in range(2)]}
    if not long_case:
        # Keep the executable path itself short; the generated command contains
        # it and the preferences path, both canonically prefixed by Windows.
        gate.preferences_path=gate.run/"p.json"
    stored=None;command=None;registry_bytes=None;registry_seed={"kind":2,"value":"%OWNED_LIFECYCLE_TEST%\\previous executable"}
    steps=[ui.step("settings","Settings"),ui.step("preferences","Application preferences"),
        ui.step("opt-in","Launch at login (local only)"),ui.step("apply-optin","Apply application preferences")]
    if not long_case:
        steps += [ui.step("opt-out","Launch at login (local only)"),ui.step("apply-optout","Apply application preferences"),
            ui.step("home","Back to Home")]
    steps[0]["wait_ms"]=12000
    def before(g):
        nonlocal stored,registry_bytes
        hwnd=g.main_hwnd()
        if not hwnd or not g.lifecycle_hwnd():return False
        if stored is None:stored=g.preferences_path.read_bytes()
        if long_case:
            if not geometry.user32.IsWindowVisible(hwnd)or geometry.user32.IsIconic(hwnd):return False
            def seed_registry():
                import winreg
                with winreg.CreateKeyEx(winreg.HKEY_CURRENT_USER,g.startup_key,0,winreg.KEY_SET_VALUE)as key:
                    winreg.SetValueEx(key,"ProtoInputMultiPlex",0,winreg.REG_EXPAND_SZ,registry_seed["value"])
                assert startup_value(g.startup_key)==registry_seed
                return registry_seed
            g.once("owned-existing-REG_EXPAND_SZ",seed_registry)
            registry_bytes=startup_raw(g.startup_key)
        else:
            if geometry.user32.IsWindowVisible(hwnd)and not geometry.user32.IsIconic(hwnd):return False
            assert not(g.run/"ui.state.json").exists()
            def restore_actual():
                action=g.native_menu("Open ProtoInputMultiPlex")
                ui.eventually(lambda:geometry.user32.IsWindowVisible(hwnd)and not geometry.user32.IsIconic(hwnd),bool)
                return action
            g.once("seven-binding-cold-actual-menu-Restore",restore_actual)
        return True
    def verify(name,sample):
        nonlocal command
        assert not(gate.run/"ui.state.json").exists(),"Preferences/startup controls started routing"
        if name=="apply-optin":
            if long_case:
                status=sample["profile_state"]["status"]
                if "not applied"not in status.lower():return False
                assert "260"in status,status
                assert gate.preferences_path.read_bytes()==stored
                assert startup_value(gate.startup_key)==registry_seed
                assert startup_raw(gate.startup_key)==registry_bytes
                state=gate.app_snapshot("ui.app-preferences.json")
                assert state["committed"]==initial
                assert not list(gate.run.glob(".*.tmp.*"))
                # Character count is reported from the exact Windows canonical
                # path form used by the producer, without writing any Run entry.
                predicted=f'"\\\\?\\{gate.images[0]}" gui --preferences "\\\\?\\{gate.preferences_path}"'
                assert len(predicted.encode("utf-16-le"))//2>260
                sample["actual_long_command_rejection"]={"visible_error":status,"previous_registry_value":startup_value(gate.startup_key),
                    "preferences_sha256":hashlib.sha256(stored).hexdigest(),"no_temporary_file":True,
                    "exact_registry_type":registry_bytes[0],"exact_registry_bytes_sha256":hashlib.sha256(registry_bytes[1]).hexdigest()}
            else:
                current=ui.read_json(gate.preferences_path)
                if not current["launch_at_login"]:return False
                actual=startup_value(gate.startup_key)
                assert actual and actual["kind"]==1
                command=validate_startup_command(actual["value"],gate.images[0],gate.preferences_path)
                units=len(command.encode("utf-16-le"))//2
                assert units<=260,(units,command)
                expected=copy.deepcopy(initial);expected["launch_at_login"]=True
                assert current==expected
                sample["valid_actual_startup_registration"]={"value":actual,"utf16_units":units}
        if name=="apply-optout":
            if ui.read_json(gate.preferences_path)["launch_at_login"]:return False
            assert startup_value(gate.startup_key)is None
            assert ui.read_json(gate.preferences_path)==initial
        return True
    def after(g):
        g.expected_exit=True
        if long_case:
            hwnd=g.main_hwnd();assert hwnd
            assert geometry.user32.PostMessageW(hwnd,0x10,0,0)
        else:g.once("startup-tail-actual-menu-Quit",lambda:g.native_menu("Quit"))
        g.gui.wait(timeout=10);assert g.gui.returncode==0
        assert not hotkeys.lifecycle_window(g.gui.pid)
        return True
    try:
        gate.execute(profile,initial,steps,verify,before,after)
        if command:
            gate.evidence["startup_command"]=command
            gate.evidence["startup_preferences_path"]=str(gate.preferences_path)
            gate.evidence["recorded_image_path"]=str(gate.images[0])
        gate.save()
        return gate.run
    finally:
        try:gate.close()
        finally:remove_startup_key(gate.startup_key)

def quarantine(args):
    Gate,ui,hotkeys,geometry=gate_type()
    gate=Gate(args,"app-lifecycle-quarantine",count=0)
    old=None;log=None;overlays=[]
    try:
        report=gate.run/"pump.json"
        fixture=Path(__file__).with_name("test-output-shutdown-timeout.py").resolve()
        log=(gate.run/"fixture.stderr.log").open("w",encoding="utf-8")
        old=subprocess.Popen([sys.executable,str(fixture),"--fixture","--report",str(report)],stderr=log,creationflags=subprocess.CREATE_NO_WINDOW)
        gate.owned.append(old)
        read=lambda:ui.read_json(report)
        initial=ui.eventually(read,lambda value:value["hwnd"]and value["frames"]>2)
        fresh=subprocess.Popen([str(gate.images[0]),"probe","--automation","--report",str(gate.run/"fresh.json")],creationflags=subprocess.CREATE_NO_WINDOW)
        gate.owned.append(fresh)
        fresh_read=lambda:ui.read_json(gate.run/"fresh.json")
        ui.eventually(fresh_read,lambda value:value["frames"]>2)
        discovery=ui.Client(gate.images[0],args.runtime.resolve(),gate.run/"discovery.stderr.log")
        try:
            inventory=ui.eventually(lambda:discovery.tool("routing_windows"),lambda value:all(any(w["pid"]==pid for w in value["windows"])for pid in(old.pid,fresh.pid)))
            windows=[next(w for w in inventory["windows"]if w["pid"]==pid)for pid in(old.pid,fresh.pid)]
            for i,w in enumerate(windows):assert ui.user32.SetWindowPos(w["hwnd"],wintypes.HWND(-1),30+i*460,40,420,360,0x10)
            inventory=discovery.tool("routing_windows")
            windows=[next(w for w in inventory["windows"]if w["pid"]==pid)for pid in(old.pid,fresh.pid)]
        finally:discovery.close()
        def profile(pid,index):
            bounds=windows[index]["client"]
            return {"version":1,"concurrent":True,"desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":[pid]},
                "seats":[{"id":"lifecycle-quarantine-own","name":"Owned recovery","mouse":"virtual://mouse/1","keyboard":"virtual://keyboard/1",
                    "desktop_start":[bounds["left"]+120,bounds["top"]+100]}]}
        first=profile(old.pid,0);second=profile(fresh.pid,1)
        ui.put(gate.run/"fresh-profile.json",second)
        preferences=preference_defaults();preferences.update(launch_minimized=False,minimize_to_tray=False,close_to_tray=False)
        preferences["shortcuts"].append({"action":"toggle_routing","key":"f7","modifiers":{"control":True,"alt":True,"shift":True,"windows":False}})
        steps=[ui.step("start","Start routing"),ui.step("settings","Settings"),ui.step("lab","Input lab"),
            ui.step("focus","1 Hold click"),ui.step("focus-up","1 Release click"),
            ui.step("hold","Hold key",{"selected_user":0,"key_name":"f5"}),ui.step("home-stop","Back to Home"),
            ui.step("blocked-start","Start routing",disabled=True),
            ui.step("settings-fresh","Settings"),ui.step("profiles-fresh","Profiles & backend"),
            ui.step("load-fresh","Load profile",{"profile_path":gate.run/"fresh-profile.json"}),ui.step("home-blocked","Back to Home"),
            ui.step("still-blocked","Start routing",disabled=True),ui.step("fresh-start","Start routing"),
            ui.step("fresh-settings","Settings"),ui.step("fresh-lab","Input lab"),ui.step("fresh-focus","1 Hold click"),ui.step("fresh-up","1 Release click"),
            ui.step("fresh-hold","Hold key",{"selected_user":0,"key_name":"f6"}),ui.step("fresh-home","Back to Home"),ui.step("fresh-stop","Stop routing")]
        def verify(name,sample):
            nonlocal overlays
            if name=="start":
                if len(sample["state"])!=1:return False
                overlays=[u["overlay"]["hwnd"]for u in sample["state"]]
            if name=="hold" and 116 not in read()["held_app_keys"]:return False
            if name=="home-stop":
                def typed_stop_failure():
                    ui.put(report.with_suffix(".mode.json"),{"mode":"peek_no_remove","milliseconds":3500})
                    blocked=ui.eventually(read,lambda value:value["pump_mode"]=="peek_no_remove")
                    sent=gate.typed_hotkey(0x5100)
                    failed=ui.eventually(gate.snapshot,lambda value:"restart"in value["status"].lower())
                    assert old.poll()is None
                    return {"typed_default_Stop":sent,"old_pid":old.pid,"blocked_pump_report":blocked,
                        "after_failed_stop":read(),"visible_status":failed["status"]}
                sample["real_failed_cleanup"]=gate.once("typed-F11-real-StopFailure",typed_stop_failure)
            if name=="blocked-start":
                def toggle_blocked():
                    sent=gate.typed_hotkey(0x5101)
                    ui.eventually(lambda:gate.snapshot()["status"],lambda text:"restart"in text.lower())
                    normal=ui.eventually(read,lambda value:value["pump_mode"]=="normal",8)
                    assert old.poll()is None
                    assert all(not ui.user32.IsWindow(h)for h in overlays)
                    return {"typed_toggle":sent,"old_process_still_alive_after_pump_resumes":True,"native_after_resume":normal}
                gate.once("quarantine-blocks-typed-toggle",toggle_blocked)
                shot=gate.screenshot("failed-cleanup-quarantine")
                if not shot:return False
                sample["owned_error_image"]=shot
            if name=="still-blocked":
                assert old.poll()is None and read()["pump_mode"]=="normal"
                assert sample["profile_state"]["profile"]["desktop"]["allowed_pids"]==[fresh.pid]
                old.terminate();old.wait(timeout=10)
                sample["exact_old_process_exit"]={"pid":old.pid,"returncode":old.returncode}
            if name=="fresh-start":
                assert old.poll()is not None
                if len(sample["state"])!=1:return False
                overlays=[u["overlay"]["hwnd"]for u in sample["state"]]
            if name=="fresh-hold" and 117 not in fresh_read()["held_keys"]:return False
            if name=="fresh-home":
                assert 117 in fresh_read()["held_keys"]
                shot=gate.screenshot("compact-home-virtual-held")
                if not shot:return False
                sample["owned_active_home_image_shared_with_UX_gate"]=shot
            if name=="fresh-stop":
                value=fresh_read()
                if len(value["key_events"])<2 or any(ui.user32.IsWindow(h)for h in overlays):return False
                assert [(e["vk"],e["pressed"])for e in value["key_events"]]==[(117,True),(117,False)]
                sample["recovery_native_key_events"]=value["key_events"]
            return True
        def after(g):
            g.expected_exit=True;hwnd=g.main_hwnd();assert hwnd
            assert geometry.user32.PostMessageW(hwnd,0x10,0,0)
            g.gui.wait(timeout=10);assert g.gui.returncode==0
            return True
        gate.execute(first,preferences,steps,verify,after_controls=after)
        return gate.run
    finally:
        gate.close()
        if log:log.close()
        remove_startup_key(gate.startup_key)

def parse_args():
    parser=argparse.ArgumentParser()
    for name in ("executable","x86-executable","runtime"):
        parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--ready",action="store_true")
    parser.add_argument("--phase",choices=("main","cold-command","quarantine","startup-long","startup-short"),default="main")
    parser.add_argument("--from-main",type=Path)
    parser.add_argument("--main-tail",action="store_true")
    parser.add_argument("--prefix",type=Path)
    parser.add_argument("--start-at",choices=("startup-on","home-profile","minimize","arm-session-only","apply-app-grant"))
    return parser.parse_args()

if __name__=="__main__":
    args=parse_args()
    assert args.ready,"Await root frozen-image READY/exclusive slot"
    assert args.phase!="cold-command"or args.from_main,"Cold command consumes exact successful main evidence"
    assert not args.main_tail or args.prefix,"Focused main tail retains exact failed prefix"
    assert not args.start_at or(args.prefix and not args.main_tail),"Continuation requires its exact accepted prefix"
    assert not args.phase.startswith("startup-")or args.prefix,"Startup tail consumes the accepted shortcut prefix"
    {"main":main_workflow,"cold-command":cold_command,"quarantine":quarantine,
        "startup-long":startup_tail,"startup-short":startup_tail}[args.phase](args)
