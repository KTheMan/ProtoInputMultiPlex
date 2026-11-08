"""Own Win32 message-pump timeout and PM_NOREMOVE queue-fence acceptance.

The fixture is an ordinary disposable application with a real WNDPROC/message
loop. It never sends input, polls keyboard state, or fabricates a product ACK.
Run each mode once, only after the producer grants the exclusive READY slot.
"""
import argparse
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

user32=ctypes.WinDLL("user32",use_last_error=True)
kernel32=ctypes.WinDLL("kernel32",use_last_error=True)
user32.PeekMessageW.argtypes=[ctypes.POINTER(wintypes.MSG),wintypes.HWND,wintypes.UINT,wintypes.UINT,wintypes.UINT]
user32.PostMessageW.argtypes=[wintypes.HWND,wintypes.UINT,wintypes.WPARAM,wintypes.LPARAM]
user32.IsWindow.argtypes=[wintypes.HWND]
user32.DispatchMessageW.argtypes=[ctypes.POINTER(wintypes.MSG)]
user32.DispatchMessageW.restype=wintypes.LPARAM
user32.TranslateMessage.argtypes=[ctypes.POINTER(wintypes.MSG)]
user32.DefWindowProcW.argtypes=[wintypes.HWND,wintypes.UINT,wintypes.WPARAM,wintypes.LPARAM]
user32.DefWindowProcW.restype=wintypes.LPARAM
user32.RegisterWindowMessageW.argtypes=[wintypes.LPCWSTR]
user32.GetWindowThreadProcessId.argtypes=[wintypes.HWND,ctypes.POINTER(wintypes.DWORD)]
WM_PUMP_MODE=0x8053
FENCE_NAME="ProtoInputMultiPlex.QueueFence.V1"
TITLE="ProtoInputMultiPlex Automation Test Owned Queue Pump"


def put(path,value):
    temporary=path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value,ensure_ascii=False),encoding="utf-8")
    deadline=time.monotonic()+1
    while True:
        try:os.replace(temporary,path);return
        except PermissionError:
            if time.monotonic()>=deadline:raise
            time.sleep(.005)


def fixture(report):
    assert ctypes.sizeof(ctypes.c_void_p)==8,"Timeout fixture must be x64"
    if hasattr(user32,"SetThreadDpiAwarenessContext"):
        user32.SetThreadDpiAwarenessContext.argtypes=[ctypes.c_void_p]
        user32.SetThreadDpiAwarenessContext(ctypes.c_void_p(-4))
    callback=ctypes.WINFUNCTYPE(wintypes.LPARAM,wintypes.HWND,wintypes.UINT,wintypes.WPARAM,wintypes.LPARAM)
    class WindowClass(ctypes.Structure):
        _fields_=[("style",wintypes.UINT),("procedure",callback),("class_extra",ctypes.c_int),("window_extra",ctypes.c_int),
                  ("instance",wintypes.HINSTANCE),("icon",wintypes.HICON),("cursor",wintypes.HANDLE),("background",wintypes.HBRUSH),
                  ("menu",wintypes.LPCWSTR),("name",wintypes.LPCWSTR)]
    state={"pid":os.getpid(),"hwnd":0,"pointer_bits":64,"pump_mode":"normal","mode_entered":0,
           "frames":0,"peek_calls":0,"peek_hits":0,"hidden_fence_copies":0,"message_counts":{},"key_events":[],"held_app_keys":[]}
    held=set();until=0.;epoch=time.perf_counter()
    @callback
    def procedure(hwnd,message,wparam,lparam):
        nonlocal until
        if message==WM_PUMP_MODE and 2000<=wparam<=10000 and lparam in (0,1):
            state["pump_mode"]="none" if lparam==0 else "peek_no_remove"
            state["mode_entered"]+=1;until=time.monotonic()+wparam/1000
            return 0
        if message in (0x100,0x101,0x104,0x105):
            down=message in (0x100,0x104)
            state["key_events"].append({"vk":int(wparam),"pressed":down,"at_us":int((time.perf_counter()-epoch)*1e6)})
            if down:held.add(int(wparam))
            else:held.discard(int(wparam))
            return 0
        if message==2:
            user32.PostQuitMessage(0);return 0
        return user32.DefWindowProcW(hwnd,message,wparam,lparam)
    kernel32.GetModuleHandleW.argtypes=[wintypes.LPCWSTR];kernel32.GetModuleHandleW.restype=wintypes.HMODULE
    instance=kernel32.GetModuleHandleW(None)
    name=f"ProtoInputMultiPlexOwnPump{os.getpid()}"
    wc=WindowClass(0,procedure,0,0,instance,None,None,wintypes.HBRUSH(6),None,name)
    user32.RegisterClassW.argtypes=[ctypes.POINTER(WindowClass)]
    assert user32.RegisterClassW(ctypes.byref(wc)),ctypes.get_last_error()
    user32.CreateWindowExW.argtypes=[wintypes.DWORD,wintypes.LPCWSTR,wintypes.LPCWSTR,wintypes.DWORD,
        ctypes.c_int,ctypes.c_int,ctypes.c_int,ctypes.c_int,wintypes.HWND,wintypes.HMENU,wintypes.HINSTANCE,ctypes.c_void_p]
    user32.CreateWindowExW.restype=wintypes.HWND
    hwnd=user32.CreateWindowExW(0x08000008,name,TITLE,0x10cf0000,70,70,420,320,None,None,instance,None)
    assert hwnd,ctypes.get_last_error();state["hwnd"]=int(hwnd)
    fence=user32.RegisterWindowMessageW(FENCE_NAME);assert fence
    message=wintypes.MSG();last_report=0.
    while True:
        now=time.monotonic()
        request=report.with_suffix(".mode.json")
        if state["mode_entered"]==0 and request.exists():
            config=json.loads(request.read_text(encoding="utf-8"))
            assert config["mode"] in ("none","peek_no_remove") and config["milliseconds"]==3500
            state["pump_mode"]=config["mode"];state["mode_entered"]+=1
            until=now+config["milliseconds"]/1000
        if state["pump_mode"]!="normal" and now>=until:state["pump_mode"]="normal"
        if state["pump_mode"]=="normal":
            for _ in range(64):
                if not user32.PeekMessageW(ctypes.byref(message),None,0,0,1):break
                kind=str(message.message);state["message_counts"][kind]=state["message_counts"].get(kind,0)+1
                if message.message==0x12:return
                user32.TranslateMessage(ctypes.byref(message));user32.DispatchMessageW(ctypes.byref(message))
                if state["pump_mode"]!="normal":break
        elif state["pump_mode"]=="peek_no_remove":
            state["peek_calls"]+=1
            # Exact message range proves the marker is really retrieved even
            # though a preceding key-up remains queued. PM_NOREMOVE is zero.
            if user32.PeekMessageW(ctypes.byref(message),hwnd,fence,fence,0):
                state["peek_hits"]+=1
                if message.message==0:state["hidden_fence_copies"]+=1
        state["frames"]+=1
        if now-last_report>=.03:
            state["held_app_keys"]=sorted(held);put(report,state);last_report=now
        time.sleep(.005)


def gate(args):
    spec=importlib.util.spec_from_file_location("timeout_client",Path(__file__).with_name("test-mcp.py"))
    helper=importlib.util.module_from_spec(spec);spec.loader.exec_module(helper)
    run=args.runtime.resolve()/time.strftime("acceptance-output-shutdown-"+args.mode+"-%Y%m%d-%H%M%S");run.mkdir()
    image=run/"control64.exe";shutil.copy2(args.executable,image)
    evidence={"result":"NOT ACCEPTED","mode":args.mode,"build_sha256":hashlib.sha256(image.read_bytes()).hexdigest(),"scope":"One own x64 Python Win32 target, virtual input only, real message retrieval/absence, bounded queue-fence timeout and late normalized release."}
    process=client=None;fixture_log=None
    try:
        report=run/"pump.json"
        fixture_log=(run/"fixture.stderr.log").open("w",encoding="utf-8")
        process=subprocess.Popen([sys.executable,str(Path(__file__).resolve()),"--fixture","--report",str(report)],creationflags=subprocess.CREATE_NO_WINDOW,stderr=fixture_log)
        read=lambda:json.loads(report.read_text(encoding="utf-8"))
        initial=helper.eventually(read,lambda s:s["hwnd"] and s["frames"]>2)
        pid=wintypes.DWORD();assert user32.GetWindowThreadProcessId(initial["hwnd"],ctypes.byref(pid)) and pid.value==process.pid
        client=helper.Client(image,args.runtime.resolve(),run/"mcp.stderr.log")
        def owned_inventory():
            assert process.poll() is None, f"Fixture exited early: {process.returncode}"
            return {"windows":[w for w in client.tool("routing_windows")["windows"] if w["pid"]==process.pid]}
        inventory=helper.eventually(owned_inventory,lambda v:any(w["pid"]==process.pid and w["title"]==TITLE for w in v["windows"]))
        window=next(w for w in inventory["windows"] if w["pid"]==process.pid and w["title"]==TITLE)
        point=[window["client"]["left"]+100,window["client"]["top"]+100]
        client.tool("routing_start",{"users":["Own timeout virtual"],"allowed_pids":[process.pid],"mode":"concurrent","multi_keyboard":True})
        client.tool("routing_input",{"action":"move_to","user":0,"x":point[0],"y":point[1]})
        for down in (True,False):client.tool("routing_input",{"action":"button","user":0,"button":"primary","pressed":down})
        settings={"buttons":[{"type":"legacy"}]*3+[{"type":"key","key":116}]+[{"type":"legacy"}],"keys":{}}
        client.tool("routing_output",{"operation":"configure","user":0,"settings":settings})
        client.tool("routing_input",{"action":"button","user":0,"button":"back","pressed":True})
        held=helper.eventually(read,lambda s:s["held_app_keys"]==[116] and len(s["key_events"])==1)
        expected="peek_no_remove" if args.mode=="peek-no-remove" else "none"
        put(report.with_suffix(".mode.json"),{"mode":expected,"milliseconds":3500})
        blocked=helper.eventually(read,lambda s:s["pump_mode"]==expected and s["mode_entered"]==1)
        begin=time.monotonic();reply=client.tool("routing_stop",error=True);elapsed=time.monotonic()-begin
        after=read();state=client.tool("routing_state")
        text=" ".join(x.get("text","") for x in reply["content"])
        assert "restart" in text.lower() and "acknowledged" in text.lower(),text
        assert state["last_error"] and "restart" in state["last_error"].lower()
        assert .5<=elapsed<3,"Shutdown did not respect its real bounded timeout"
        assert process.poll() is None and user32.IsWindow(initial["hwnd"])
        assert after["pump_mode"]==expected and after["held_app_keys"]==[116] and len(after["key_events"])==1
        if args.mode=="peek-no-remove":
            assert after["peek_hits"]>0 and after["hidden_fence_copies"]>0,"No actual PM_NOREMOVE fence observations"
        resumed=helper.eventually(read,lambda s:s["pump_mode"]=="normal" and len(s["key_events"])>=2,8)
        assert [(e["vk"],e["pressed"]) for e in resumed["key_events"]]==[(116,True),(116,False)],resumed["key_events"]
        assert not resumed["held_app_keys"]
        evidence.update(result="PASS",before_hold=held,blocked=blocked,stop_error=reply,stop_state=state,stop_seconds=elapsed,after_timeout=after,after_resume=resumed)
    except Exception as error:
        evidence["failure"]=repr(error);raise
    finally:
        if client:
            try:client.close()
            except Exception:client.process.kill();client.process.wait(timeout=10)
        if process:
            if process.poll() is None:process.terminate()
            process.wait(timeout=10)
        if fixture_log:fixture_log.close()
        evidence["owned_processes_closed"]=not process or process.poll() is not None
        (run/"evidence.json").write_text(json.dumps(evidence,indent=2),encoding="utf-8")
        print(f"{evidence['result']}: {run}",flush=True)


def main():
    if "--fixture" in sys.argv:
        parser=argparse.ArgumentParser();parser.add_argument("--fixture",action="store_true");parser.add_argument("--report",type=Path,required=True)
        fixture(parser.parse_args().report);return
    parser=argparse.ArgumentParser()
    parser.add_argument("--executable",type=Path,required=True);parser.add_argument("--runtime",type=Path,required=True)
    parser.add_argument("--mode",choices=("none","peek-no-remove"),required=True);parser.add_argument("--ready",action="store_true")
    args=parser.parse_args();assert args.ready,"Wait for producer READY/exclusive native slot";gate(args)

if __name__=="__main__":main()
