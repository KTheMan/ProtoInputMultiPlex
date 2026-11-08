"""Three owned native-overlay baseline samples; explicit frozen ROOT READY required."""
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


def module(name,filename):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(filename))
    value=importlib.util.module_from_spec(spec);spec.loader.exec_module(value);return value


def digest(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def run(args):
    repo=Path(__file__).resolve().parent.parent
    producer=repo/"runtime/producer-p1-target-exit-20261015-024710/verification.json"
    assert digest(producer)=="c44632cd654b2a8d5312fd1a88a232856e38a593fd894a6c4a20012e0a0a55e6"
    frozen=json.loads(producer.read_text(encoding="utf-8"))
    assert frozen["result"]=="PASS"and frozen["workspace_tests"]==186
    assert len(frozen["source_after_sha256"])==64
    assert all(digest(repo/path)==expected for path,expected in frozen["source_after_sha256"].items())
    assert digest(args.executable)==frozen["build_sha256"]["x64"]
    assert digest(args.x86_executable)==frozen["build_sha256"]["i686"]
    current=module("unicode_current_client","test-mcp.py")
    capture=module("unicode_owned_capture","test-cursor-layout-acceptance.py")
    # Only Client/eventually and capture_owned are used: no historical gate start/schema helper.
    assert capture.user32.SetThreadDpiAwarenessContext(ctypes.c_void_p(-4))
    folder=args.runtime.resolve()/time.strftime("acceptance-native-cursor-unicode-baseline-%Y%m%d-%H%M%S")
    folder.mkdir();image=folder/"control64.exe";shutil.copy2(args.executable,image)
    provenance=folder/"runner-source";provenance.mkdir()
    for name in(Path(__file__).name,"test-mcp.py","test-cursor-layout-acceptance.py"):
        shutil.copy2(Path(__file__).with_name(name),provenance/name)
    evidence={"result":"NOT ACCEPTED","scope":"Native overlay glyph baseline only; no all-Unicode or fallback acceptance.",
        "producer":{"path":str(producer),"sha256":digest(producer)},"build_sha256":frozen["build_sha256"],
        "groups":{},"samples":{},"tool_calls":[],"optional_i686":"verified metadata only; never launched"}
    client=None;probes=[];windows=[];overlays=set()
    def save():(folder/"evidence.json").write_text(json.dumps(evidence,ensure_ascii=False,indent=2)+"\n",encoding="utf-8")
    def call(name,args=None):
        result=client.tool(name,args)
        evidence["tool_calls"].append({"name":name,"arguments":args or{},"result":result});save();return result
    def reports():
        return current.eventually(lambda:[json.loads((folder/f"probe-{i}.json").read_text(encoding="utf-8"))for i in range(2)],lambda value:True,3)
    def state():return call("routing_state")
    def overlay(user):
        result=state();assert result["running"]and not result.get("last_error")
        o=copy.deepcopy(result["users"][user]["overlay"]);hwnd=o["hwnd"]
        assert hwnd and capture.user32.IsWindow(hwnd)
        rect=capture.Rect();assert capture.user32.GetWindowRect(hwnd,ctypes.byref(rect))
        actual=[rect.left,rect.top,rect.right,rect.bottom]
        o["geometry_matches"]=(actual[:2]==o["render_origin"]
            and [rect.right-rect.left,rect.bottom-rect.top]==o["canvas_size"]
            and [rect.left+o["hotspot"][0],rect.top+o["hotspot"][1]]==[o["x"],o["y"]])
        assert o["painted_pixels"]>0 and o["no_activate"]and o["click_through"]
        o["native_window_rect"]=actual;o["native_visible"]=bool(capture.user32.IsWindowVisible(hwnd))
        overlays.add(hwnd);return result,o
    def sample(name,user,font):
        candidate=copy.deepcopy(defaults[user]);candidate.update(label=True,label_font_size=float(font),size=32,
            badge=None,visible=True,idle_hide_ms=None,lock_window=False)
        previous_generation=state()["users"][user]["overlay"]["paint_generation"]
        result=call("routing_settings",{"user":user,"settings":candidate})
        assert next(entry["settings"]for entry in result["user_settings"]if entry["user"]==user)==candidate
        def rendered():
            value,o=overlay(user)
            actual=next(entry["settings"]for entry in value["user_settings"]if entry["user"]==user)
            return value,o,actual
        value,o,actual=current.eventually(rendered,lambda v:v[2]==candidate and v[1]["visible"]
            and v[1]["native_visible"]and v[1]["geometry_matches"]
            and v[1]["rendered_label"]==names[user]and not v[1]["label_truncated"]
            and v[1]["paint_generation"]>previous_generation,10)
        assert o["rendered_label"] and "\ufffd"not in o["rendered_label"]
        assert not o["label_truncated"]and o["rendered_label"]==names[user]
        assert [u["pid"]for u in value["users"]]==pids
        pixels=capture.capture_owned(o["hwnd"],folder/f"overlay-{name}.png")
        assert pixels["size"]==o["canvas_size"]
        evidence["samples"][name]={"user":user,"source_name":names[user],"font_size":font,
            "settings":candidate,"pre_settings_paint_generation":previous_generation,"worker_user":value["users"][user],"overlay":o,"owned_PrintWindow":pixels,
            "png_sha256":digest(pixels["path"]),"glyph_verdict":"PENDING DIRECT ORIGINAL REVIEW"};save()
    names=["Alpha ASCII 123","漢字 日本語 かな カナ 한글"]
    save()
    try:
        for i in range(2):
            probes.append(subprocess.Popen([str(image),"probe","--automation","--report",str(folder/f"probe-{i}.json")],creationflags=subprocess.CREATE_NO_WINDOW))
        pids=[process.pid for process in probes]
        client=current.Client(image,args.runtime.resolve(),folder/"mcp.stderr.log")
        def own_inventory():
            inventory=client.tool("routing_windows")["windows"]
            return [next((w for w in inventory if w["pid"]==pid and w["title"].startswith("ProtoInputMultiPlex Automation Test")),None)for pid in pids]
        windows=current.eventually(own_inventory,lambda value:all(value),20)
        for i,w in enumerate(windows):assert current.user32.SetWindowPos(w["hwnd"],wintypes.HWND(-1),40+500*i,40,440,360,0x10)
        windows=current.eventually(own_inventory,lambda value:all(value),10)
        evidence["owned_targets"]=windows;save()
        started=call("routing_start",{"users":names,"allowed_pids":pids,"mode":"concurrent","multi_keyboard":True})
        started=current.eventually(state,lambda value:value["running"]and len(value["users"])==2 and all(u["overlay"]["hwnd"]for u in value["users"])and len({u["overlay"]["hwnd"]for u in value["users"]})==2 and all(capture.user32.IsWindow(u["overlay"]["hwnd"])for u in value["users"]),15)
        overlays.update(u["overlay"]["hwnd"]for u in started["users"])
        defaults=[copy.deepcopy(next(entry["settings"]for entry in started["user_settings"]if entry["user"]==i))for i in range(2)]
        for i,w in enumerate(windows):
            call("routing_input",{"action":"move_to","user":i,"x":w["client"]["left"]+160,"y":w["client"]["top"]+120})
            for pressed in(True,False):call("routing_input",{"action":"button","user":i,"button":"primary","pressed":pressed})
        selected=current.eventually(state,lambda v:[u["pid"]for u in v["users"]]==pids and [u["keyboard_pid"]for u in v["users"]]==pids,10)
        current.eventually(reports,lambda rs:all([x["pressed"]for x in r["mouse_events"]]==[True,False]for r in rs),10)
        evidence["groups"]["owned_recipient_selection"]={"state":selected};save()
        sample("latin-14",0,14);sample("mixed-14",1,14);sample("mixed-32",1,32)
        evidence["groups"]["three_native_baselines"]={"samples":list(evidence["samples"]),"no_automatic_glyph_pass":True};save()
        for i,key in enumerate(("f6","f7")):call("routing_input",{"action":"key","user":i,"key":key,"pressed":True})
        current.eventually(reports,lambda rs:117 in rs[0]["held_keys"]and 118 in rs[1]["held_keys"],10)
        stopped=call("routing_stop")
        assert not stopped["running"]
        final=current.eventually(reports,lambda rs:all(not r["held_keys"]and len(r["key_events"])==2 for r in rs),10)
        for i,r in enumerate(final):
            assert [event["pressed"]for event in r["key_events"]]==[True,False]
            assert all(event["vk"]==117+i for event in r["key_events"])
            assert r["key_events"][-1]["scan"]==64+i and not r["key_events"][-1]["extended"]and not r["key_events"][-1]["alt"]
        current.eventually(lambda:all(not capture.user32.IsWindow(hwnd)for hwnd in overlays),bool,10)
        evidence["groups"]["ordinary_clean_stop"]={"stopped":stopped,"native_reports":final,"known_overlay_hwnds":sorted(overlays)}
        evidence.update(result="PENDING INDEPENDENT GLYPH REVIEW",capture_geometry_native_result="PASS");save()
    except Exception as error:
        evidence.update(result="NOT ACCEPTED",failure=repr(error));save();raise
    finally:
        if client:
            try:client.close()
            except Exception as error:
                evidence["client_cleanup_error"]=repr(error)
                if client.process.poll()is None:client.process.kill();client.process.wait(timeout=12)
                client.stderr.close()
        evidence["known_overlays_closed"]=all(not capture.user32.IsWindow(hwnd)for hwnd in overlays)
        current.user32.PostMessageW.argtypes=[wintypes.HWND,wintypes.UINT,ctypes.c_size_t,ctypes.c_ssize_t]
        for i,process in enumerate(probes):
            if process.poll()is None:
                if i<len(windows)and windows[i]:
                    current.user32.PostMessageW(windows[i]["hwnd"],0x10,0,0)
                    try:process.wait(timeout=5)
                    except subprocess.TimeoutExpired:process.terminate()
                else:process.terminate()
            process.wait(timeout=12)
        evidence["owned_processes_closed"]=all(p.poll()is not None for p in probes)and(client is None or client.process.poll()is not None)
        if evidence.get("client_cleanup_error")or not evidence["owned_processes_closed"]or not evidence["known_overlays_closed"]:
            evidence["result"]="NOT ACCEPTED"
        save();print(f"{evidence['result']}: {folder}",flush=True)
    return folder


def main():
    parser=argparse.ArgumentParser();parser.add_argument("--ready",action="store_true")
    for name in("executable","x86-executable","runtime"):parser.add_argument("--"+name,type=Path,required=True)
    args=parser.parse_args();assert args.ready,"Await frozen ROOT READY/exclusive native slot"
    run(args)


if __name__=="__main__":main()
