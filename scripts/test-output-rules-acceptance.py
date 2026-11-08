"""Finite output conversion/ownership gate. Launch only after producer READY."""
import argparse
import copy
import hashlib
import importlib.util
from pathlib import Path
import time

def module(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value

_base = module("output_base", "test-automation-acceptance.py")
_studio = module("output_studio_helpers", "test-macro-studio-acceptance.py")
_trace = module("output_native_trace", "output-rules-fixture-helpers.py")
eventually = _base.eventually

def rules(buttons=None, keys=None):
    result = {"buttons": [{"type": "legacy"} for _ in range(5)], "keys": {}}
    for source, destination in (buttons or {}).items():
        result["buttons"][source] = {"type": "disabled"} if destination is None else {"type": "key", "key": destination}
    for source, destination in (keys or {}).items():
        result["keys"][str(source)] = {"type": "disabled"} if destination is None else {"type": "key", "key": destination}
    return result


class OutputGate(_base.Gate):
    fixture_count = 2
    run_prefix = "acceptance-output-rules"
    tool_names = _base._client_module.EXPECTED_TOOLS | {"routing_output"}

    def output(self, user=None, value=None, error=False):
        request = {"operation": "inspect"} if user is None else {"operation": "configure", "user": user, "settings": value}
        result = self.client.tool("routing_output", request, error=error)
        if not error:
            state = result["output"]
            assert state and not state["error"], state
            if user is not None:
                assert next(u["settings"] for u in state["users"] if u["user"] == user) == value
        return result

    def start(self):
        super().start()
        self.evidence["build_sha256"] = {str(p): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in (self.run / "control64.exe", self.run / "probe32.exe")}
        assert all(u["settings"] == rules() for u in self.output()["output"]["users"])

    def mouse(self, user, button, down):
        return self.input("button", user=user, button=button, pressed=down)

    def wait_edges(self, baseline, user, expected):
        eventually(self.reports, lambda rs: len([e for e in baseline.new_events(rs, user) if e["kind"] == "key"]) == len(expected))
        baseline.expect_keys(self.reports(), user, expected)

    def delivery(self):
        for user in range(2):
            self.output(user, rules({3: 116, 4: None}, {118: 119, 120: None}))
            baseline = _trace.NativeBaseline(self.reports())
            self.mouse(user, "back", True)
            self.mouse(user, "back", False)
            self.tap(user, "f7")
            self.mouse(user, "forward", True)
            self.mouse(user, "forward", False)
            self.tap(user, "f9")
            self.wait_edges(baseline, user, [(116, True), (116, False), (119, True), (119, False)])
            baseline.expect_no_button_delivery(self.reports(), user)
            baseline.unchanged_other_target(self.reports(), 1-user)
            # Legacy still uses cursor button mapping, including swap behavior.
            cursor = copy.deepcopy(self.state()["user_settings"][user]["settings"])
            cursor["swap_buttons"] = True
            self.client.tool("routing_settings", {"user": user, "settings": cursor})
            count = self.report(user)["mouse_event_count"]
            self.mouse(user, "primary", True)
            self.mouse(user, "primary", False)
            sample = eventually(lambda: self.report(user), lambda r: r["mouse_event_count"] == count+2)
            assert sample["mouse_events"][-2:] == [{"button": 1, "pressed": True}, {"button": 1, "pressed": False}]
            cursor["swap_buttons"] = False
            self.client.tool("routing_settings", {"user": user, "settings": cursor})
            self.output(user, rules())
        self.released()
        self.snapshot("x64-x86-native-conversion-remap-disabled-and-legacy-map")

    def union(self):
        self.output(0, rules({3: 116, 4: 116}, {118: 116}))
        baseline = _trace.NativeBaseline(self.reports())
        self.mouse(0, "back", True)
        self.wait_edges(baseline, 0, [(116, True)])
        self.mouse(0, "forward", True)
        self.key(0, "f7", True)
        self.key(0, "f5", True)
        eventually(lambda: self.report(0), lambda r: 116 in r["held_keys"])
        self.mouse(0, "back", False)
        self.key(0, "f7", False)
        self.mouse(0, "forward", False)
        time.sleep(.2)
        baseline.expect_keys(self.reports(), 0, [(116, True)])
        assert 116 in self.report(0)["held_keys"]
        self.key(0, "f5", False)
        self.wait_edges(baseline, 0, [(116, True), (116, False)])
        baseline.expect_no_button_delivery(self.reports(), 0)
        self.snapshot("two-mouse-aliases-remapped-key-and-manual-vk-union")

    def live_change(self):
        self.output(0, rules({3: 116, 4: 117}, {118: 119}))
        self.mouse(0, "back", True)
        self.mouse(0, "forward", True)
        self.key(0, "f7", True)
        self.key(1, "f4", True)
        eventually(self.reports, lambda rs: {116,117,119} <= set(rs[0]["held_keys"]) and 115 in rs[1]["held_keys"])
        baseline = _trace.NativeBaseline(self.reports())
        self.output(0, rules({3: 120, 4: 117}, {118: 119}))
        self.wait_edges(baseline, 0, [(116, False)])
        assert {117,119} <= set(self.report(0)["held_keys"]) and 115 in self.report(1)["held_keys"]
        self.mouse(0, "back", False)  # Must not emit a release for newly configured F9.
        time.sleep(.2)
        baseline.expect_keys(self.reports(), 0, [(116, False)])
        self.mouse(0, "back", True)
        self.mouse(0, "back", False)
        self.wait_edges(baseline, 0, [(116, False), (120, True), (120, False)])
        self.snapshot("live-change-retires-only-changed-pair-and-old-up-cannot-release-new-code")
        self.mouse(0, "forward", False)
        self.key(0, "f7", False)
        self.key(1, "f4", False)
        self.released()

    def origins(self):
        self.output(0, rules({3: 116}))
        # Play intentionally overrides same-user Manual input. Use the other
        # virtual seat at the shared keyboard target for a valid cross-origin union.
        self.client.tool("routing_mode",{"mode":"concurrent","multi_keyboard":False})
        self.key(1, "f5", True)
        self.key(1, "f6", True)
        held = _studio.clip(_studio.event(0, _studio.button(0, True, 3)),
            _studio.event(0, _studio.key(0, True, vk=115, scan=62)),
            _studio.event(6_000_000, _studio.text(0, "late")))
        self.client.tool("macro_studio", {"operation": "load", "user": 0, "clip": held})
        self.client.tool("macro_studio", {"operation": "play", "user": 0, "options": {"loops": 1, "speed": 1, "delay_ms": 0, "stop_on_escape": True, "keep_awake": False}})
        eventually(self.state, lambda s: s["macro_studio"]["users"][0]["playback"]["events_emitted"] >= 2)
        eventually(lambda:self.report(1),lambda r:{115,116,117} <= set(r["held_keys"]))
        baseline = _trace.NativeBaseline(self.reports())
        self.output(0, rules({3: 117}))
        assert self.state()["macro_studio"]["users"][0]["playback"]["state"] == "playing"
        assert {116,115,117} <= set(self.report(1)["held_keys"])
        self.evidence["studio_change_before_cancel"]={"state":self.state(),"probes":self.reports()};self.save()
        self.client.tool("macro_studio", {"operation": "cancel", "user": 0})
        time.sleep(.2)
        self.wait_edges(baseline,1,[(115,False)])
        assert {116,117} <= set(self.report(1)["held_keys"]) and 115 not in self.report(1)["held_keys"]
        baseline.expect_keys(self.reports(),0,[])
        self.evidence["studio_scoped_cancel"]={"state":self.state(),"probes":self.reports()};self.save()
        self.key(1,"f5",False);self.key(1,"f6",False);self.released()
        self.client.tool("routing_mode",{"mode":"concurrent","multi_keyboard":True})
        self.output(0, rules({3: 116}))
        self.client.tool("automation_clip", {"action": "load", "clip": held})
        self.client.tool("automation_play", {"loops": 1, "speed": 1})
        eventually(self.state, lambda s: s["automation"]["playback"]["events_emitted"] >= 2)
        eventually(lambda:self.report(0),lambda r:{115,116} <= set(r["held_keys"]))
        composite=_trace.NativeBaseline(self.reports())
        self.output(0,rules({3:117}))
        self.wait_edges(composite,0,[(116,False)])
        assert self.state()["automation"]["playback"]["state"]=="playing" and 115 in self.report(0)["held_keys"]
        self.evidence["composite_change_before_global_cancel"]={"state":self.state(),"probes":self.reports()};self.save()
        self.client.tool("automation_cancel")  # Existing documented global cancel.
        self.released()
        self.wait_edges(composite,0,[(116,False),(115,False)])
        self.snapshot("studio-changed-rule-and-scoped-cancel-preserve-manual-union-composite-global-cancel")

    def raw_graph(self):
        self.output(0, rules({3: 116}))
        self.output(1, rules({3: 117}))
        self.client.tool("routing_replication", {"edges": [{"source":0,"target":1,"mouse":True,"keyboard":False,"text":False}]})
        baseline = _trace.NativeBaseline(self.reports())
        self.client.tool("automation_record", {"recording":True})
        self.mouse(0, "back", True)
        self.mouse(0, "back", False)
        clip = self.client.tool("automation_record", {"recording":False})["automation"]["clip"]
        assert len(clip["events"]) == 2
        assert [(e["action"]["packet"]["kind"],e["action"]["packet"]["flags"]) for e in clip["events"]] == [(0,64),(0,128)]
        assert all(e["action"]["user"]==0 for e in clip["events"])
        self.wait_edges(baseline, 0, [(116,True),(116,False)])
        self.wait_edges(baseline, 1, [(117,True),(117,False)])
        for user in range(2): baseline.expect_no_button_delivery(self.reports(),user)
        self.client.tool("automation_clip", {"action":"load","clip":clip})
        self.client.tool("automation_play", {"loops":1,"speed":1})
        eventually(self.state,lambda s:s["automation"]["playback"]["state"]=="completed")
        self.wait_edges(baseline,0,[(116,True),(116,False)]*2)
        self.wait_edges(baseline,1,[(117,True),(117,False)]*2)
        self.key(1,"f6",True)
        eventually(lambda:self.report(1),lambda r:117 in r["held_keys"])
        union = _trace.NativeBaseline(self.reports())
        self.mouse(0,"back",True); self.mouse(0,"back",False)
        self.wait_edges(union,0,[(116,True),(116,False)])
        time.sleep(.2)
        union.expect_keys(self.reports(),1,[])
        assert 117 in self.report(1)["held_keys"]
        self.key(1,"f6",False)
        self.wait_edges(union,1,[(117,False)])
        self.client.tool("routing_replication", {"edges":[{"source":0,"target":1,"mouse":False,"keyboard":True,"text":False}]})
        filtered = _trace.NativeBaseline(self.reports())
        self.mouse(0,"back",True); self.mouse(0,"back",False)
        self.wait_edges(filtered,0,[(116,True),(116,False)])
        time.sleep(.2)
        filtered.unchanged_other_target(self.reports(),1)
        self.client.tool("routing_replication", {"edges":[]})
        self.snapshot("raw-mouse-recording-playback-and-destination-local-replication-category")

    def invalid(self):
        self.output(0,rules({3:116}))
        self.key(0,"f5",True); self.key(1,"f6",True)
        eventually(self.reports,lambda rs:116 in rs[0]["held_keys"] and 117 in rs[1]["held_keys"])
        held=_studio.clip(_studio.event(0,_studio.button(0,True,3)),_studio.event(6_000_000,_studio.text(0,"late")))
        self.client.tool("macro_studio",{"operation":"load","user":0,"clip":held})
        key_count=self.report(0)["key_event_count"]
        self.client.tool("macro_studio",{"operation":"play","user":0,"options":{"loops":1,"speed":1,"delay_ms":0,"stop_on_escape":True,"keep_awake":False}})
        eventually(self.state,lambda s:s["macro_studio"]["users"][0]["playback"]["events_emitted"]>=1)
        # The logical hold stays true across Manual UP -> Studio DOWN; wait on
        # delivered window edges, not that indistinguishable sampled boolean.
        startup=eventually(lambda:self.report(0),lambda r:r["key_event_count"]==key_count+2)
        assert [(e["vk"],e["pressed"]) for e in startup["key_events"][-2:]]==[(116,False),(116,True)]
        baseline = _trace.NativeBaseline(self.reports())
        accepted = self.output()["output"]["users"]
        for candidate in (rules({3:0}),rules({3:255}),rules(keys={7:116}),rules(keys={118:255}),
                          {"buttons":[{"type":"legacy"}],"keys":{}}, {**rules(),"unknown":True}):
            self.output(0,candidate,error=True)
            assert self.output()["output"]["users"]==accepted
        self.output(99,rules(),error=True)
        time.sleep(.2)
        baseline.expect_keys(self.reports(),0,[]); baseline.expect_keys(self.reports(),1,[])
        baseline.unchanged_holds(self.reports(),(0,1))
        assert self.state()["macro_studio"]["users"][0]["playback"]["state"]=="playing"
        self.snapshot("invalid-output-rules-reject-before-held-input-mutation")
        self.client.tool("macro_studio",{"operation":"cancel","user":0})
        self.key(0,"f5",False); self.key(1,"f6",False); self.released()

    def five_buttons(self):
        for user in range(2):
            self.output(user,rules({source:116+source for source in range(5)}))
            baseline=_trace.NativeBaseline(self.reports());expected=[]
            for source,name in enumerate(("primary","secondary","middle","back","forward")):
                self.mouse(user,name,True);self.mouse(user,name,False)
                expected += [(116+source,True),(116+source,False)]
                self.wait_edges(baseline,user,expected)
            baseline.expect_no_button_delivery(self.reports(),user)
            baseline.unchanged_other_target(self.reports(),1-user)
            self.output(user,rules())
        self.released();self.snapshot("all-five-source-buttons-to-distinct-native-keys-x64-x86")


def main():
    parser=argparse.ArgumentParser()
    for name in ("executable","x86-executable","runtime"):
        parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--ready",action="store_true")
    parser.add_argument("--tail",action="store_true",help="Live-change onward after accepted delivery/union prefix")
    parser.add_argument("--remaining",action="store_true",help="Origin/recording/rejection/five-button/cleanup only after accepted live-change prefix")
    parser.add_argument("--final-only",action="store_true",help="Rejection/five-button/cleanup after accepted origin/recording prefix")
    args=parser.parse_args(); assert args.ready,"No native launch before producer READY"
    if args.tail or args.remaining or args.final_only:OutputGate.run_prefix="acceptance-output-rules-tail"
    gate=OutputGate(args)
    try:
        gate.start()
        if not (args.tail or args.remaining or args.final_only):gate.delivery();gate.union()
        if not (args.remaining or args.final_only):gate.live_change()
        if not args.final_only:gate.origins();gate.raw_graph()
        gate.invalid();gate.five_buttons()
        hwnds=[u["overlay"]["hwnd"] for u in gate.state()["users"]]
        gate.client.tool("routing_stop"); gate.released()
        eventually(lambda:[_base.user32.IsWindow(h) for h in hwnds],lambda live:not any(live))
        gate.snapshot("normal-stop-native-release-and-overlay-cleanup")
        gate.evidence["result"]="PASS"
    except Exception as error:
        gate.evidence.update(result="NOT ACCEPTED",failure=repr(error)); raise
    finally:
        gate.close(); print(f"{gate.evidence.get('result','NOT ACCEPTED')}: {gate.run}",flush=True)

if __name__=="__main__": main()
