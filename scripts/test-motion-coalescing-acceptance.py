"""Draft finite publication-batching gate; do not launch before frozen ROOT READY."""
import argparse
import copy
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import time


def module(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def freeze(args):
    assert args.ready, "Await final source review/producer/ROOT READY"
    assert digest(args.producer) == args.producer_sha256.lower()
    producer = json.loads(args.producer.read_text(encoding="utf-8"))
    assert producer["result"] == "PASS" and producer["source_before_sha256"] == producer["source_after_sha256"]
    assert len(producer["checks"]) == 5 and all(c["exit_code"] == 0 for c in producer["checks"])
    repo = Path(__file__).resolve().parent.parent
    assert all(digest(repo / p) == h for p, h in producer["source_after_sha256"].items())
    assert digest(args.executable) == producer["build_sha256"]["x64"]
    assert digest(args.x86_executable) == producer["build_sha256"]["i686"]
    assert all(digest(args.runtime / p) == h for p, h in producer["runtime_sha256"].items())
    return producer


def packet(user, flags=0, x=0, y=0, data=0, kind=0):
    return {"type": "packet", "user": user,
            "packet": {"device": 0xF1000002 + 2*user + kind,
                       "kind": kind, "flags": flags, "x": x, "y": y, "data": data}}



def native_prefix(args):
    assert args.from_run and args.from_run_sha256
    prior = args.from_run.resolve()
    assert prior.parent == args.runtime.resolve() and prior.name == "acceptance-motion-coalescing-20261015-071258"
    assert args.from_run_sha256.lower() == "67c9fb43d5eddf1f185bd0455ea220d8c3aafafdab0986c10528df766ad48ce3"
    assert digest(prior/"evidence.json") == args.from_run_sha256.lower()
    assert digest(prior/"independent-review.json") == "d3edceeb6fcfcabd04ff9895fd2567635daace0258f02232910143ecb3188f9c"
    assert digest(prior/"runner-source/test-motion-coalescing-acceptance.py") == "860a96c422b4e2893bfafa20aa4d0b1a88957dba2e7a9bee6ff7d47e5b6ae292"
    previous = json.loads((prior/"evidence.json").read_text(encoding="utf-8"))
    review = json.loads((prior/"independent-review.json").read_text(encoding="utf-8"))
    assert previous["result"] == "NOT ACCEPTED" and review["decision"] == "NOT ACCEPTED aggregate; bounded prefix APPROVED"
    assert previous["owned_processes_closed"] and previous["known_overlays_closed"]
    assert previous["producer_sha256"] == args.producer_sha256.lower()
    assert previous["virtual_packet_budget_used"] == 47
    assert sum(c["name"] not in ("routing_state", "routing_windows") for c in previous["calls"]) == 18
    assert [(g["group"],g["native_motion_counts"],g["logical_deltas"]) for g in review["approved_prefix"]] == [
        ("default_off_baseline",[8,4],[[24,0],[4,0]]),
        ("count_first_and_per_user",[2,4],[[24,0],[4,0]]),
        ("buffer_only_safety_mode",[2,0],[[24,0],[0,0]])]
    return prior, previous


def native(args, producer):
    continuation = args.phase == "native_tail"
    prefix_path, prefix = native_prefix(args) if continuation else (None, None)
    ui = module("coalescing_client", "test-mcp.py")
    geometry = module("coalescing_geometry", "owned-gui-geometry.py")
    geometry.user32.PostMessageW.argtypes=[wintypes.HWND,wintypes.UINT,ctypes.c_size_t,ctypes.c_ssize_t]
    folder = args.runtime.resolve() / time.strftime("acceptance-motion-coalescing-%Y%m%d-%H%M%S")
    folder.mkdir()
    image = folder / "control64.exe"
    shutil.copy2(args.executable, image)
    source = folder / "runner-source"
    source.mkdir()
    shutil.copy2(Path(__file__), source / Path(__file__).name)
    evidence = {"result": "NOT ACCEPTED", "phase": args.phase, "groups": {}, "calls": [],
                "producer": str(args.producer), "producer_sha256": digest(args.producer),
                "build_sha256": producer["build_sha256"], "virtual_packet_budget_used": 0}
    if continuation:
        evidence["prefix"] = {"path":str(prefix_path),"raw_sha256":digest(prefix_path/"evidence.json"),
                              "review_sha256":digest(prefix_path/"independent-review.json"),
                              "setup":"Fresh two-target session is setup only; no baseline/count/per-user/buffer-only replay."}
    client, probes, windows, layers = None, [], [], set()
    mutations = 0

    def save():
        (folder / "evidence.json").write_text(json.dumps(evidence, indent=2)+"\n", encoding="utf-8")

    def call(name, arguments=None, error=False, source_packets=0):
        nonlocal mutations
        if name not in ("routing_windows", "routing_state"):
            mutations += 1
        evidence["virtual_packet_budget_used"] += source_packets
        assert mutations <= (40 if continuation else 64) and evidence["virtual_packet_budget_used"] <= (40 if continuation else 96)
        record = {"name": name, "arguments": arguments or {}, "started_ns": time.monotonic_ns(),
                  "source_virtual_packets": source_packets, "expected_error": error}
        evidence["calls"].append(record)
        save()
        record["result"] = client.tool(name, arguments, error=error)
        record["finished_ns"] = time.monotonic_ns()
        save()
        return record["result"]

    def state():
        return call("routing_state")

    def reports():
        return ui.eventually(lambda: [json.loads((folder/f"probe-{i}.json").read_text(encoding="utf-8")) for i in range(2)],
                             lambda value: True, 3)

    def settings(index, candidate):
        result = call("routing_settings", {"user": index, "settings": candidate})
        assert next(u["settings"] for u in result["user_settings"] if u["user"] == index) == candidate
        return result

    def input_action(action, **fields):
        return call("routing_input", {"action": action, **fields},
                    source_packets=int(action in ("motion", "button", "wheel", "key")))

    def quiescent():
        # Readonly polling cannot be the barrier that publishes buffered motion.
        until = time.monotonic() + 5
        previous, stable = None, 0
        while time.monotonic() < until:
            value, rs = state(), reports()
            assert value["running"] and not value.get("last_error"), value
            counts = [r["native_event_count"] for r in rs]
            stable = stable + 1 if counts == previous else 0
            previous = counts
            matched = all(not r["held_keys"] and not r["buttons"] for r in rs)
            for user, process in enumerate(probes):
                if process.poll() is not None:
                    continue
                motions = [event for event in rs[user]["native_events"] if event["kind"] == "motion"]
                matched &= bool(motions) and motions[-1]["client"] == value["users"][user]["client"]
            if matched and stable >= 2:
                return value, rs
            time.sleep(.05)
        raise AssertionError("Native publication endpoint did not settle without another input")

    def program(label, actions):
        before_state, before_reports = quiescent()
        clip = {"version": 1, "events": [{"at_micros": 0, "action": a} for a in actions]}
        call("automation_clip", {"action": "load", "clip": clip})
        call("automation_play", {"loops": 1, "speed": 1.0}, source_packets=len(actions))
        ui.eventually(state, lambda s: s["automation"]["playback"] is not None
                      and s["automation"]["playback"]["state"] == "completed"
                      and s["automation"]["playback"]["events_emitted"] == len(actions), 5)
        after_state, after_reports = quiescent()
        traces = [after_reports[i]["native_events"][len(before_reports[i]["native_events"]):] for i in range(2)]
        assert all(r["native_event_count"] == len(r["native_events"]) for r in after_reports), "Trace capacity reached"
        result = {"schedule": clip, "before_state": before_state, "after_state": after_state,
                  "before_reports": before_reports, "after_reports": after_reports,
                  "native_traces": traces, "motion_counts": [sum(e["kind"] == "motion" for e in trace) for trace in traces],
                  "logical_deltas": [[after_state["users"][i]["client"][j]-before_state["users"][i]["client"][j] for j in (0,1)] for i in range(2)]}
        evidence["groups"][label] = result
        save()
        return result

    try:
        for i in range(2):
            probes.append(subprocess.Popen([str(image), "probe", "--automation", "--report", str(folder/f"probe-{i}.json")],
                                           creationflags=subprocess.CREATE_NO_WINDOW))
        pids = [p.pid for p in probes]
        client = ui.Client(image, args.runtime.resolve(), folder / "mcp.stderr.log")
        evidence["process_ids"]={"probes":pids,"mcp":client.process.pid}
        save()

        def own_windows():
            inventory = client.tool("routing_windows")["windows"]
            return [next((w for w in inventory if w["pid"] == pid and w["title"].startswith("ProtoInputMultiPlex Automation Test")), None) for pid in pids]

        windows = ui.eventually(own_windows, lambda ws: all(ws), 15)
        for i, w in enumerate(windows):
            assert ui.user32.SetWindowPos(w["hwnd"], wintypes.HWND(-1), 40+480*i, 50, 440, 360, 0x10)
        windows = ui.eventually(own_windows, lambda ws: all(ws), 5)
        started = call("routing_start", {"users": ["Coalescing A", "Coalescing B"], "allowed_pids": pids,
                                         "mode": "concurrent", "multi_keyboard": True})
        started = ui.eventually(state, lambda s: s["running"] and len(s["users"]) == 2 and all(u.get("overlay", {}).get("hwnd") for u in s["users"]), 10)
        layers.update(u["overlay"]["hwnd"] for u in started["users"])
        assert len(layers) == 2 and all(h and ui.user32.IsWindow(h) for h in layers)
        defaults = [copy.deepcopy(next(s["settings"] for s in started["user_settings"] if s["user"] == i)) for i in range(2)]
        assert all(s["motion_coalescing"] == {"buffer_events": None, "interval_ms": None} for s in defaults)
        for i, w in enumerate(windows):
            input_action("move_to", user=i, x=w["client"]["left"]+140, y=w["client"]["top"]+120)
            for down in (True, False):
                input_action("button", user=i, button="primary", pressed=down)
        selected = ui.eventually(state, lambda s: [u["pid"] for u in s["users"]] == pids, 5)
        evidence["owned_targets"] = windows
        a = copy.deepcopy(defaults[0]); a.update(speed_x=1.5, speed_y=.75, motion_curve="gentle")
        if continuation:
            baseline = prefix["groups"]["default_off_baseline"]
            pair_delta = [d//4 for d in baseline["logical_deltas"][0]]
            assert pair_delta == [6,0]
            a["motion_coalescing"] = {"buffer_events":4,"interval_ms":None}
            settings(0,a)
            barrier_cases = [
                ("button_barrier",[packet(0,flags=1),packet(0,flags=2)],[("button",True),("button",False)]),
                ("key_barrier",[packet(0,kind=1,x=64,data=117),packet(0,kind=1,flags=1,x=64,data=117)],[("key",True),("key",False)]),
                ("wheel_barrier",[packet(0,flags=0x400,data=120)],[("wheel",None)])]
            for label,discrete_actions,expected_order in barrier_cases:
                group = program(label,[packet(0,x=1,y=1),packet(0,x=3,y=-1)]+discrete_actions)
                trace = group["native_traces"][0]
                assert [(e["kind"],e["pressed"])for e in trace if e["kind"]!="motion"] == expected_order
                endpoint = [group["before_state"]["users"][0]["client"][j]+pair_delta[j]for j in(0,1)]
                assert group["logical_deltas"] == [pair_delta,[0,0]]
                for index,event in enumerate(trace):
                    if event["kind"] == "motion":continue
                    preceding = [e for e in trace[:index]if e["kind"]=="motion"]
                    assert preceding and preceding[-1]["client"]==endpoint,(label,event,preceding,endpoint)
                    if event["client"]is not None:assert event["client"]==endpoint
                # All trace duplicates retained. No later motion follows the discrete
                # segment in this clip, so dequeue-time filter state stays at endpoint.
        else:
            settings(0, a)
            mixed = []
            for i in range(8):
                mixed.append(packet(0, x=1 if i%2 == 0 else 3, y=1 if i%2 == 0 else -1))
            mixed.extend(packet(1,x=1)for _ in range(4))
            baseline = program("default_off_baseline", mixed)
            a["motion_coalescing"] = {"buffer_events": 4, "interval_ms": 16}
            settings(0, a)
            coalesced = program("count_first_and_per_user", mixed)
            assert 0 < coalesced["motion_counts"][0] < baseline["motion_counts"][0]
            assert coalesced["motion_counts"][1] == baseline["motion_counts"][1]
            assert coalesced["logical_deltas"] == baseline["logical_deltas"]
            a["motion_coalescing"] = {"buffer_events": 4, "interval_ms": None}
            settings(0, a)
            only = program("buffer_only_safety_mode", [action for action in mixed if action["user"] == 0])
            assert 0 < only["motion_counts"][0] < baseline["motion_counts"][0]
            assert only["logical_deltas"][0] == baseline["logical_deltas"][0]
            barrier_actions = [packet(0,x=1,y=1), packet(0,x=3,y=-1), packet(0,flags=1), packet(0,flags=2),
                               packet(0,x=1,y=1), packet(0,x=3,y=-1), packet(0,kind=1,x=64,data=117), packet(0,kind=1,flags=1,x=64,data=117),
                               packet(0,x=1,y=1), packet(0,x=3,y=-1), packet(0,flags=0x400,data=120)]
            barriers = program("button_key_wheel_barriers", barrier_actions)
            trace = barriers["native_traces"][0]
            discrete = [e for e in trace if e["kind"] != "motion"]
            assert [(e["kind"],e["pressed"]) for e in discrete] == [("button",True),("button",False),("key",True),("key",False),("wheel",None)]
            assert all(delta%4==0 for delta in baseline["logical_deltas"][0])
            pair_delta=[delta//4 for delta in baseline["logical_deltas"][0]]
            discrete_index=0
            for index, event in enumerate(trace):
                if event["kind"] != "motion":
                    preceding = [e for e in trace[:index] if e["kind"] == "motion"]
                    assert preceding, "Discrete event preceded pending endpoint publication"
                    pairs=(1,1,2,2,3)[discrete_index]
                    expected=[barriers["before_state"]["users"][0]["client"][j]+pairs*pair_delta[j] for j in(0,1)]
                    assert preceding[-1]["client"]==expected,(event,preceding[-1],expected)
                    if event["client"] is not None:
                        assert preceding[-1]["client"] == event["client"]
                    discrete_index+=1
            assert barriers["logical_deltas"][0]==[3*d for d in pair_delta]
        a["motion_coalescing"] = {"buffer_events": None, "interval_ms": 4}
        settings(0, a)
        before = quiescent()
        mutation_before=mutations
        packet_before=evidence["virtual_packet_budget_used"]
        input_action("motion", user=0, dx=2 if continuation else 1, dy=0 if continuation else 1)
        after = quiescent()
        source_count=evidence["virtual_packet_budget_used"]-packet_before
        assert mutations==mutation_before+1 and source_count==1 and source_count<64
        if continuation:
            # Magnitude2 is below Gentle threshold3: 2*speed1.5 gives exactly3;
            # carried subunit remainder cannot change the integer result.
            expected_delta=[3,0]
        else:
            reference=[event for event in baseline["native_traces"][0]if event["kind"]=="motion"][0]["client"]
            expected_delta=[reference[j]-baseline["before_state"]["users"][0]["client"][j]for j in(0,1)]
        assert expected_delta != [0,0]
        expected_endpoint=[before[0]["users"][0]["client"][j]+expected_delta[j]for j in(0,1)]
        assert after[0]["users"][0]["client"]==expected_endpoint
        timer_trace=after[1][0]["native_events"][len(before[1][0]["native_events"]):]
        timer_motions=[event for event in timer_trace if event["kind"]=="motion"]
        assert timer_motions and timer_motions[-1]["client"]==expected_endpoint
        evidence["groups"]["timer_without_new_input"] = {"before": before, "after": after,
                    "expected_delta":expected_delta,"new_native_trace":timer_trace,"expected_endpoint":expected_endpoint,"source_packet_count":source_count,
                    "claim": "Native endpoint converged with readonly polling only; exact timer deadline is production-unit evidence, not a latency benchmark."}
        for bad in ({"buffer_events":1,"interval_ms":None},{"buffer_events":None,"interval_ms":17}):
            candidate = copy.deepcopy(a); candidate["motion_coalescing"] = bad
            call("routing_settings", {"user":0,"settings":candidate}, error=True)
            assert next(x["settings"] for x in state()["user_settings"] if x["user"] == 0) == a
        transitions = {}
        input_action("motion", user=0, dx=3, dy=1)
        a["motion_coalescing"] = {"buffer_events":64,"interval_ms":16}
        settings(0, a)
        transitions["settings"] = quiescent()
        input_action("motion", user=0, dx=3, dy=1)
        paused = input_action("pause")
        assert paused["paused"]
        resumed = input_action("resume")
        assert not resumed["paused"]
        transitions["pause_resume"] = quiescent()
        b = copy.deepcopy(defaults[1]);b["motion_coalescing"] = {"buffer_events":9,"interval_ms":5}
        settings(1,b)
        input_action("motion",user=0,dx=3,dy=1)
        assert geometry.user32.PostMessageW(windows[0]["hwnd"],0x10,0,0)
        probes[0].wait(timeout=8)
        assert probes[0].returncode == 0
        survived = ui.eventually(state,lambda s:s["running"] and s["users"][0]["pid"]==0 and s["users"][1]["pid"]==pids[1],5)
        before_keys = len(reports()[1]["key_events"])
        for down in(True,False):input_action("key",user=1,key="f7",pressed=down)
        rs = ui.eventually(reports,lambda rs:len(rs[1]["key_events"])>=before_keys+2,5)
        assert [(x["vk"],x["pressed"])for x in rs[1]["key_events"][before_keys:]]==[(118,True),(118,False)]
        transitions["target_exit_B_continuity"] = {"state":survived,"reports":rs}
        input_action("motion",user=1,dx=2,dy=1)
        stopped=call("routing_stop")
        assert not stopped["running"]
        ui.eventually(lambda:all(not ui.user32.IsWindow(h)for h in layers),bool,8)
        final=reports();assert all(not r["held_keys"] and not r["buttons"]for r in final)
        transitions["stop"]={"result":stopped,"reports":final}
        evidence["groups"]["lifecycle_boundaries"] = transitions
        evidence["groups"]["configuration_validation"] = {"MCP_B":b["motion_coalescing"],"invalid_candidates":2}
        evidence.update(result="PENDING INDEPENDENT DATA REVIEW",mutating_calls=mutations,
                        lifecycle_limit="No public pending stats. Exact still-pending transition interleavings source/unit only unless the trace naturally establishes them.")
        (folder/"gui-coalescing-payload.json").write_text(json.dumps(b["motion_coalescing"]),encoding="utf-8")
    except Exception as error:
        evidence.update(result="NOT ACCEPTED",failure=repr(error))
        raise
    finally:
        if client:
            try:client.close()
            except Exception as error:
                evidence["client_cleanup_error"]=repr(error)
                if client.process.poll()is None:client.process.kill();client.process.wait(timeout=12)
                client.stderr.close()
        for i,process in enumerate(probes):
            if process.poll()is None:
                if i<len(windows)and windows[i]:geometry.user32.PostMessageW(windows[i]["hwnd"],0x10,0,0)
                try:process.wait(timeout=5)
                except subprocess.TimeoutExpired:process.terminate();process.wait(timeout=10)
        evidence["owned_processes_closed"]=all(p.poll()is not None for p in probes) and(client is None or client.process.poll()is not None)
        evidence["known_overlays_closed"]=all(not ui.user32.IsWindow(h)for h in layers)
        if evidence.get("client_cleanup_error") or not evidence["owned_processes_closed"] or not evidence["known_overlays_closed"]:
            evidence["result"]="NOT ACCEPTED"
        save()
        print(f"{evidence['result']}: {folder}",flush=True)


def gui(args, producer):
    assert args.from_run and args.from_run_sha256
    prior = args.from_run.resolve()
    assert prior.parent == args.runtime.resolve() and prior.name.startswith("acceptance-motion-coalescing-")
    assert digest(prior/"evidence.json") == args.from_run_sha256.lower()
    native_evidence = json.loads((prior/"evidence.json").read_text(encoding="utf-8"))
    assert native_evidence["result"] == "PENDING INDEPENDENT DATA REVIEW"
    assert native_evidence["phase"] == "native_tail"
    assert args.from_review_sha256 and digest(prior/"independent-review.json") == args.from_review_sha256.lower()
    accepted = json.loads((prior/"independent-review.json").read_text(encoding="utf-8"))
    assert accepted["decision"] == "APPROVED native combination"
    assert accepted["tail_raw_sha256"] == digest(prior/"evidence.json")
    assert accepted["prefix_raw_sha256"] == native_evidence["prefix"]["raw_sha256"]
    assert accepted["prefix_review_sha256"] == native_evidence["prefix"]["review_sha256"]
    assert native_evidence["owned_processes_closed"] and native_evidence["known_overlays_closed"]
    assert native_evidence["producer_sha256"] == args.producer_sha256.lower()
    payload = native_evidence["groups"]["configuration_validation"]["MCP_B"]
    assert payload == {"buffer_events":9,"interval_ms":5}
    lifecycle = module("coalescing_gui_lifecycle", "test-app-lifecycle-ui.py")
    comparison = module("coalescing_strict_json", "test-keyboard-persistence-ui.py")
    Base, ui, hotkeys, geometry = lifecycle.gate_type()

    class Gate(Base):
        @staticmethod
        def assert_event_contract(steps, events):
            assert len(events)<=len(steps)
            pairs=0
            for spec,event in zip(steps,events):
                if spec.get("key"):
                    assert event["button"]==spec["button"] and event["key"]==spec["key"]
                    assert event["focused"] and not event["clicked"]
                    assert 1<=event["key_pairs"]<=24
                    if spec["key"]!="Tab":assert event["key_pairs"]==1 and event["changed"]
                    pairs+=event["key_pairs"]
                else:Base.assert_event_contract([spec],[event])
            assert pairs<=48

    g=Gate(args,"motion-coalescing-gui",offline=True,count=0)
    output=g.run/"fresh coalescing profile.json"
    assert not output.exists()
    # Stale PIDs remain inert profile data: this GUI never starts a worker.
    pids=[w["pid"]for w in native_evidence["owned_targets"]]
    profile={"version":1,"concurrent":True,
             "desktop":{"mode":"concurrent","multi_keyboard":True,"allowed_pids":pids},
             "seats":[{"id":"coalescing-"+letter,"name":"Coalescing "+letter.upper(),
                       "mouse":f"virtual://mouse/{index+1}","keyboard":f"virtual://keyboard/{index+1}",
                       "cursor":{"lock_window":False,"motion_coalescing":{"buffer_events":4,"interval_ms":16}if index==0 else {"buffer_events":None,"interval_ms":None}}}
                      for index,letter in enumerate(("a","b"))]}
    preferences={"version":1,"appearance_mode":"dark","zoom_percent":100,"launch_minimized":False,
                 "minimize_to_tray":False,"close_to_tray":False,"launch_at_login":False,"shortcuts":[]}
    baseline=None;accepted_profile=None;prior_save_status=None

    def key(name,label,key):
        step=ui.step(name,label);step["key"]=key;return step

    steps=[ui.step("select-b","Select user 2"),ui.step("configure","Configure user"),ui.step("motion","Motion"),
           ui.step("count-on","Coalesce by event count"),key("count-focus","Buffered event limit","Tab"),
           key("count-plus","Buffered event limit","ArrowRight"),ui.step("time-on","Coalesce by time window"),
           key("time-focus","Coalescing time window","Tab"),key("time-plus","Coalescing time window","ArrowRight"),
           ui.step("home","Back to Home"),ui.step("settings","Settings"),
           ui.step("users","Settings category Users & devices"),ui.step("profiles","Profiles & backend"),
           ui.step("save","Save profile",{"profile_path":output})]
    assert len(steps)==14
    try:
        source=g.run/"runner-source";source.mkdir()
        shutil.copy2(Path(__file__),source/Path(__file__).name)
        g.evidence.update(producer=str(args.producer),producer_sha256=digest(args.producer),native_prefix=str(prior),
                          native_prefix_sha256=digest(prior/"evidence.json"),scope="Stopped14-observation configuration only; no routing/probes/UIA.")
        def verify(name,sample):
            nonlocal baseline,accepted_profile,prior_save_status
            actual=sample["profile_state"]["profile"]
            if baseline is None:baseline=copy.deepcopy(actual);accepted_profile=copy.deepcopy(actual)
            expected=copy.deepcopy(accepted_profile)
            transitions={"count-on":{"buffer_events":8,"interval_ms":None},
                         "count-plus":{"buffer_events":9,"interval_ms":None},
                         "time-on":{"buffer_events":9,"interval_ms":4},"time-plus":payload}
            if name in transitions:expected["seats"][1]["cursor"]["motion_coalescing"]=transitions[name]
            if actual!=expected:
                comparison.strict_equal(actual,accepted_profile)
                return False
            comparison.strict_equal(actual,expected)
            assert ui.read_json(g.preferences_path)==preferences and not sample["state"]
            if name=="count-plus":
                shot=g.screenshot("count-normal")
                if not shot:return False
                assert shot["geometry"]["client_logical"]==[540.0,520.0]
                sample["owned_original"]=shot
                g.once("minimum-size",lambda:geometry.resize_owned_client(g.main_hwnd(),[320,360]))
            if name=="time-plus":
                shot=g.screenshot("time-minimum")
                if not shot:return False
                assert shot["geometry"]["client_logical"]==[320.0,360.0]
                sample["owned_original"]=shot
            if name=="profiles":
                assert not output.exists()
                prior_save_status=sample["profile_state"]["status"]
            if name=="save":
                status=sample["profile_state"]["status"]
                if status==prior_save_status:return False
                assert status==f"Saved {output}. No previous file existed to back up.",status
                assert sample["profile_state"]["active_path"]==str(output)
                if not output.exists()or not output.stat().st_size:return False
                saved=json.loads(output.read_text(encoding="utf-8"))
                comparison.strict_equal(saved,actual)
                assert saved["seats"][1]["cursor"]["motion_coalescing"]==payload
                assert not list(g.run.glob("*.tmp.*"))
                g.evidence["saved_profile"]={"path":str(output),"sha256":digest(output),"value":saved}
            accepted_profile=copy.deepcopy(expected)
            return True
        def after(gate):
            gate.expected_exit=True
            assert geometry.user32.PostMessageW(gate.main_hwnd(),0x10,0,0)
            gate.gui.wait(timeout=10)
            assert gate.gui.returncode==0 and not hotkeys.lifecycle_window(gate.gui.pid)
            return True
        g.execute(profile,preferences,steps,verify,after_controls=after)
        g.evidence["result"]="PENDING INDEPENDENT GUI DATA/PIXEL REVIEW"
    finally:
        g.close();lifecycle.remove_startup_key(g.startup_key);g.save()


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument("--ready",action="store_true")
    for name in("executable","x86-executable","runtime","producer"):parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--producer-sha256",required=True)
    parser.add_argument("--phase",choices=("native","native_tail","gui"),required=True)
    parser.add_argument("--from-run",type=Path)
    parser.add_argument("--from-run-sha256")
    parser.add_argument("--from-review-sha256")
    args=parser.parse_args()
    producer=freeze(args)
    if args.phase in("native","native_tail"):native(args,producer)
    else:gui(args,producer)


if __name__ == "__main__":
    main()
