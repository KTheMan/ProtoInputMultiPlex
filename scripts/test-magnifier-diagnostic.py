"""One finite owned-fixture diagnostic for PrintWindow versus MCP latency."""
import argparse
import ctypes
from ctypes import wintypes
import importlib.util
from pathlib import Path
import time

spec=importlib.util.spec_from_file_location("mag_gate",Path(__file__).with_name("test-magnifier-acceptance.py"))
module=importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
parser=argparse.ArgumentParser()
parser.add_argument("--executable",type=Path,required=True)
parser.add_argument("--x86-executable",type=Path,required=True)
parser.add_argument("--runtime",type=Path,required=True)
parser.add_argument("--no-redirection",action="store_true")
args=parser.parse_args()
module.MagnifierGate.run_prefix="acceptance-magnifier-diagnostic"
gate=module.MagnifierGate(args)
gdi=ctypes.WinDLL("gdi32",use_last_error=True)
user32=module.user32
gdi.CreateCompatibleDC.argtypes=[wintypes.HDC]
gdi.CreateCompatibleDC.restype=wintypes.HDC
gdi.CreateCompatibleBitmap.argtypes=[wintypes.HDC,ctypes.c_int,ctypes.c_int]
gdi.CreateCompatibleBitmap.restype=wintypes.HBITMAP
gdi.SelectObject.argtypes=[wintypes.HDC,wintypes.HGDIOBJ]
gdi.SelectObject.restype=wintypes.HGDIOBJ
gdi.DeleteObject.argtypes=[wintypes.HGDIOBJ]
gdi.DeleteDC.argtypes=[wintypes.HDC]
user32.GetDC.argtypes=[wintypes.HWND]
user32.GetDC.restype=wintypes.HDC
user32.ReleaseDC.argtypes=[wintypes.HWND,wintypes.HDC]
user32.PrintWindow.argtypes=[wintypes.HWND,wintypes.HDC,wintypes.UINT]
original_style=None
root_hwnd=None
try:
    gate.start()
    gate.discover_view()
    if args.no_redirection:
        root=next(w for w in gate.client.tool("routing_windows")["windows"] if w["pid"]==gate.probes[0].pid and w["title"].startswith("ProtoInputMultiPlex Automation Test"))
        root_hwnd=root["hwnd"]
        original_style=user32.GetWindowLongPtrW(root_hwnd,-20)
        user32.SetWindowLongPtrW(root_hwnd,-20,original_style|0x00200000)
        assert user32.GetWindowLongPtrW(root_hwnd,-20)&0x00200000
        assert user32.SetWindowPos(root_hwnd,None,0,0,0,0,0x10|0x20|0x1|0x2|0x4)
    gate.set_delay(0,2500)
    hwnd=gate.report(0)["capture_hwnd"]
    screen=user32.GetDC(hwnd)
    dc=gdi.CreateCompatibleDC(screen)
    bitmap=gdi.CreateCompatibleBitmap(screen,339,236)
    old=gdi.SelectObject(dc,bitmap)
    try:
        started=time.monotonic()
        result=user32.PrintWindow(hwnd,dc,3)
        native_elapsed=time.monotonic()-started
    finally:
        gdi.SelectObject(dc,old)
        gdi.DeleteObject(bitmap)
        gdi.DeleteDC(dc)
        user32.ReleaseDC(hwnd,screen)
    started=time.monotonic()
    response=gate.configure_view(0,mode="lens",width=64,height=64,auto_refresh=False)
    configure_elapsed=time.monotonic()-started
    state=gate.magnifier()
    gate.evidence["diagnostic"]={"no_redirection_experiment":args.no_redirection,"printwindow_flags":3,"printwindow_result":result,"printwindow_seconds":native_elapsed,"configure_seconds":configure_elapsed,"immediate_magnifier_state":state,"delay_report":gate.report(0)}
    gate.configure_view(0)
    gate.set_delay(0,0)
    print({"run":str(gate.run),**gate.evidence["diagnostic"]},flush=True)
finally:
    if original_style is not None and user32.IsWindow(root_hwnd):
        user32.SetWindowLongPtrW(root_hwnd,-20,original_style)
        user32.SetWindowPos(root_hwnd,None,0,0,0,0,0x10|0x20|0x1|0x2|0x4)
    gate.close()
