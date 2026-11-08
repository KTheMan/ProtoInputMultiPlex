"""Finite conditional vertical-to-horizontal gate, virtual owned targets only.

No launch before the producer READY/exclusive slot. Native message telemetry is
the outcome oracle; runtime state only establishes program/queue synchronization.
"""
import argparse
import copy
import importlib.util
import time
from pathlib import Path

def module(name,file):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(file))
    value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value

wheel=module("conditional_wheel_base","test-horizontal-wheel-acceptance.py")
output=module("conditional_output_helpers","test-output-rules-acceptance.py")
studio=module("conditional_studio_helpers","test-macro-studio-acceptance.py")
eventually=wheel.eventually
BUTTONS=("primary","secondary","middle","back","forward")
MODES={"never","always",*("while_"+b for b in BUTTONS)}

class ConditionalGate(wheel.WheelGate):
    run_prefix="acceptance-conditional-wheel"
    def start(self):
        super().start()
        self.user_defaults=[u["settings"] for u in self.state()["user_settings"]]
        assert all(s["horizontal_wheel_emulation"]=="never" for s in self.user_defaults)
        schema=next(t["inputSchema"] for t in self.evidence["tools"] if t["name"]=="routing_settings")
        def resolve(value):
            while "$ref" in value:
                ref=value["$ref"];assert ref.startswith("#/")
                value=schema
                for part in ref[2:].split("/"):value=value[part.replace("~1","/").replace("~0","~")]
            return value
        settings=resolve(schema["properties"]["settings"])
        assert set(resolve(settings["properties"]["horizontal_wheel_emulation"])["enum"])==MODES
        self.evidence["selected_groups"]=self.args.groups.split(",")
    def apply(self,user,**changes):
        value=copy.deepcopy(self.user_defaults[user]);value.update(changes)
        result=self.client.tool("routing_settings",{"user":user,"settings":value})
        assert next(u["settings"] for u in result["user_settings"] if u["user"]==user)==value
    def output(self,user,buttons):
        value=output.rules(buttons)
        result=self.client.tool("routing_output",{"operation":"configure","user":user,"settings":value})
        assert next(u["settings"] for u in result["output"]["users"] if u["user"]==user)==value
    def button(self,user,name,down):self.input("button",user=user,button=name,pressed=down)
    def axis(self,user,raw_axis,delta,native_axis,expected=None,isolated=True,modifiers=None):
        before=self.reports();protected=self.protected(self.state())
        self.input("wheel",user=user,axis=raw_axis,delta=delta)
        if native_axis is None:
            time.sleep(.2);after=self.report(user)
            assert all(after[k]==before[user][k] for k in ("wheel_events","horizontal_wheel_events"))
        else:
            field="horizontal_wheel_events" if native_axis=="horizontal" else "wheel_events"
            other="wheel_events" if native_axis=="horizontal" else "horizontal_wheel_events"
            after=eventually(lambda:self.report(user),lambda r:r[field]==before[user][field]+1)
            assert after[other]==before[user][other]
            kind="horizontal_wheel" if native_axis=="horizontal" else "wheel"
            event=next(e for e in reversed(after["native_events"]) if e["kind"]==kind)
            assert event["delta"]==expected and event["wheel_screen"]==list(self.points[user]),event
            if modifiers is not None:assert event["wheel_modifiers"]==modifiers,event
            assert after["native_event_count"]<4096,"Fixture native trace truncated"
        assert after["cursor"]==before[user]["cursor"]
        assert after["key_event_count"]==before[user]["key_event_count"] and after["mouse_event_count"]==before[user]["mouse_event_count"]
        if isolated:
            other_report=self.report(1-user)
            for key in ("wheel_events","horizontal_wheel_events","key_event_count","mouse_event_count","cursor"):
                assert other_report[key]==before[1-user][key],("other target",key)
        assert self.protected(self.state())==protected,"Wheel changed recipient/root/capture/held state"
    def conditions(self):
        self.axis(0,"vertical",120,"vertical",120)
        # Disabled output isolates RAW triggers from all native mouse bits.
        self.output(0,{i:None for i in range(5)})
        for button in BUTTONS:
            self.apply(0,horizontal_wheel_emulation="while_"+button)
            self.axis(0,"vertical",120,"vertical",120)
            self.button(0,button,True);self.axis(0,"vertical",120,"horizontal",120)
            self.button(0,button,False);self.axis(0,"vertical",120,"vertical",120)
        self.apply(1,horizontal_wheel_emulation="always",wheel_scale=.5,invert_wheel=True,horizontal_wheel_scale=2,invert_horizontal_wheel=True)
        self.axis(1,"vertical",120,"horizontal",-240)
        self.axis(1,"vertical",-120,"horizontal",240)
        self.axis(1,"vertical",0,"horizontal",0)
        self.axis(1,"horizontal",120,None)
        self.apply(1,horizontal_wheel_emulation="always",horizontal_wheel="convert_to_vertical",horizontal_wheel_scale=.5,invert_horizontal_wheel=True,wheel_scale=2)
        self.axis(1,"horizontal",120,"vertical",-60)
        self.output(0,{});self.apply(0);self.apply(1)
        self.snapshot("raw-five-button-conditions-default-always-once-math-and-separate-incoming-axis")
    def ownership(self):
        self.output(0,{})
        mapping=copy.deepcopy(self.user_defaults[0]["button_mapping"]);mapping[3]=None
        self.apply(0,horizontal_wheel_emulation="while_back",button_mapping=mapping)
        self.button(0,"back",True);self.axis(0,"vertical",120,"horizontal",120,modifiers=0)
        self.button(0,"back",False);self.axis(0,"vertical",120,"vertical",120)
        self.apply(0,horizontal_wheel_emulation="while_primary",swap_buttons=True)
        self.button(0,"primary",True)
        eventually(lambda:self.report(0),lambda r:r["buttons"]==2)
        self.axis(0,"vertical",120,"horizontal",120,modifiers=2)
        self.button(0,"primary",False);self.axis(0,"vertical",120,"vertical",120)
        self.output(0,{3:116});self.apply(0,horizontal_wheel_emulation="while_back")
        self.button(0,"back",True);eventually(lambda:self.report(0),lambda r:116 in r["held_keys"])
        self.axis(0,"vertical",120,"horizontal",120)
        self.button(0,"back",False);eventually(lambda:self.report(0),lambda r:116 not in r["held_keys"])
        self.axis(0,"vertical",120,"vertical",120)
        for user in (0,1):self.output(user,{3:None})
        self.apply(0);self.apply(1,horizontal_wheel_emulation="while_back")
        self.client.tool("routing_replication",{"edges":[{"source":0,"target":1,"mouse":True,"keyboard":False,"text":False}]})
        program=studio.clip(studio.event(0,studio.button(0,True,3)),studio.event(6_000_000,studio.text(0,"not emitted")))
        self.client.tool("macro_studio",{"operation":"load","user":0,"clip":program})
        self.client.tool("macro_studio",{"operation":"play","user":0,"options":{"loops":1,"speed":1,"delay_ms":0,"stop_on_escape":True,"keep_awake":False}})
        eventually(self.state,lambda s:s["macro_studio"]["users"][0]["playback"]["events_emitted"]==1)
        self.axis(1,"vertical",120,"horizontal",120)
        self.button(1,"back",True);self.key(1,"f6",True)
        eventually(lambda:self.report(1),lambda r:117 in r["held_keys"])
        self.client.tool("macro_studio",{"operation":"cancel","user":0})
        self.axis(1,"vertical",120,"horizontal",120)
        assert 117 in self.report(1)["held_keys"]
        self.button(1,"back",False);self.axis(1,"vertical",120,"vertical",120)
        self.key(1,"f6",False);self.client.tool("routing_replication",{"edges":[]})
        self.snapshot("key-output-trigger-and-replica-studio-manual-two-origin-first-down-last-up")
    def graph(self,**options):
        source,target=[u["id"] for u in self.state()["identities"]]
        defaults={"motion":False,"wheel":True,"horizontal_wheel":False,"buttons":[False]*5}
        defaults.update(options)
        self.client.tool("routing_graph",{"operation":"apply","graph":{"version":1,"edges":[{"source":source,"target":target,"mouse":True,"keyboard":False,"text":False,"options":defaults}]}})
    def pipeline(self):
        self.apply(0,horizontal_wheel_emulation="while_back");self.apply(1)
        for user in (0,1):self.output(user,{3:None})
        self.graph();before=self.reports()
        self.client.tool("automation_record",{"recording":True})
        self.button(0,"back",True);self.axis(0,"vertical",120,"horizontal",120,isolated=False);self.button(0,"back",False)
        clip=self.client.tool("automation_record",{"recording":False})["automation"]["clip"]
        assert [e["action"]["packet"]["flags"] for e in clip["events"]]==[64,0x400,128]
        eventually(lambda:self.report(1),lambda r:r["wheel_events"]==before[1]["wheel_events"]+1)
        self.client.tool("automation_clip",{"action":"load","clip":clip});self.client.tool("automation_play",{"loops":1,"speed":1})
        eventually(self.state,lambda s:s["automation"]["playback"]["state"]=="completed")
        eventually(self.reports,lambda rs:rs[0]["horizontal_wheel_events"]==before[0]["horizontal_wheel_events"]+2 and rs[1]["wheel_events"]==before[1]["wheel_events"]+2)
        assert self.report(1)["horizontal_wheel_events"]==before[1]["horizontal_wheel_events"]
        self.graph(wheel=False,horizontal_wheel=True)
        self.button(0,"back",True);self.axis(0,"vertical",120,"horizontal",120);self.button(0,"back",False)
        self.graph(step_interval_ms=1000)
        self.apply(0,horizontal_wheel_emulation="always");self.apply(1,horizontal_wheel_emulation="while_back")
        self.button(1,"back",True);baseline=self.report(1)
        self.axis(0,"vertical",120,"horizontal",120,isolated=False)
        eventually(lambda:self.report(1),lambda r:r["horizontal_wheel_events"]==baseline["horizontal_wheel_events"]+1)
        self.axis(0,"vertical",120,"horizontal",120,isolated=False)
        pending=eventually(self.state,lambda s:s["automation"]["replication"]["pending_events"]>0)
        self.button(1,"back",False)
        delivered=eventually(lambda:self.report(1),lambda r:r["wheel_events"]==baseline["wheel_events"]+1)
        assert delivered["horizontal_wheel_events"]==baseline["horizontal_wheel_events"]+1
        assert next(e for e in reversed(delivered["native_events"]) if e["kind"]=="wheel")["delta"]==120
        eventually(self.state,lambda s:s["automation"]["replication"]["pending_events"]==0)
        self.client.tool("routing_replication",{"edges":[]})
        self.evidence["queued_destination_trigger_pending_count"]=pending["automation"]["replication"]["pending_events"]
        self.snapshot("raw-record-replay-axis-categories-and-destination-condition-at-paced-delivery")
    def lifecycle(self):
        self.apply(0,horizontal_wheel_emulation="while_back");self.output(0,{3:116})
        self.button(0,"back",True);self.key(1,"f6",True)
        eventually(self.reports,lambda rs:116 in rs[0]["held_keys"] and 117 in rs[1]["held_keys"])
        self.output(0,{3:None});eventually(lambda:self.report(0),lambda r:116 not in r["held_keys"])
        assert 117 in self.report(1)["held_keys"]
        self.axis(0,"vertical",120,"vertical",120);self.button(0,"back",False)
        self.button(0,"back",True);self.axis(0,"vertical",120,"horizontal",120)
        self.apply(0,horizontal_wheel_emulation="while_back",horizontal_wheel_scale=2)
        self.axis(0,"vertical",120,"vertical",120);assert 117 in self.report(1)["held_keys"]
        self.button(0,"back",False);self.key(1,"f6",False)
        self.button(0,"back",True);self.input("pause");self.input("resume")
        self.axis(0,"vertical",120,"vertical",120)
        self.button(0,"back",True);self.client.tool("routing_mode",{"mode":"switched","multi_keyboard":True})
        self.axis(0,"vertical",120,"vertical",120)
        self.client.tool("routing_mode",{"mode":"concurrent","multi_keyboard":True})
        self.button(0,"back",True);self.input("release_all");self.axis(0,"vertical",120,"vertical",120)
        self.snapshot("changed-rule-settings-pause-mode-release-clean-raw-triggers-and-preserve-other-owner")
    def boundaries(self):
        self.apply(0,horizontal_wheel_emulation="while_back");self.output(0,{3:None});self.button(0,"back",True)
        accepted=self.state()["user_settings"]
        candidate=copy.deepcopy(accepted[0]["settings"]);candidate["horizontal_wheel_emulation"]="sometimes"
        self.client.tool("routing_settings",{"user":0,"settings":candidate},error=True)
        self.client.tool("routing_settings",{"user":99,"settings":accepted[0]["settings"]},error=True)
        assert self.state()["user_settings"]==accepted
        self.axis(0,"vertical",120,"horizontal",120);self.button(0,"back",False)
        self.snapshot("invalid-configuration-retains-current-raw-trigger-and-session")

def main():
    parser=argparse.ArgumentParser()
    for name in ("executable","x86-executable","runtime"):parser.add_argument("--"+name,type=Path,required=True)
    parser.add_argument("--ready",action="store_true")
    parser.add_argument("--groups",default="conditions,ownership,pipeline,lifecycle,boundaries")
    args=parser.parse_args();assert args.ready,"Wait for root READY and exclusive live slot"
    gate=ConditionalGate(args)
    try:
        gate.start()
        for group in args.groups.split(","):
            assert group in ("conditions","ownership","pipeline","lifecycle","boundaries"),group
            getattr(gate,group)()
        overlays=[u["overlay"]["hwnd"] for u in gate.state()["users"]]
        gate.client.tool("routing_stop");gate.released()
        eventually(lambda:[wheel._base.user32.IsWindow(h) for h in overlays],lambda values:not any(values))
        gate.snapshot("stop-owned-overlay-and-input-cleanup");gate.evidence["result"]="PASS"
    except Exception as error:gate.evidence.update(result="NOT ACCEPTED",failure=repr(error));raise
    finally:gate.close();print(f"{gate.evidence.get('result')}: {gate.run}",flush=True)

if __name__=="__main__":main()
