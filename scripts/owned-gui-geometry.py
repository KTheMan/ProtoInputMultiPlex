"""Acceptance-only geometry and product-frame capture for a known owned GUI PID.

No desktop capture, OS input injection, title inventory or unrelated app actions.
The caller supplies the PID it just launched and the product's own screenshot.
"""
import ctypes
from ctypes import wintypes
import hashlib
import io
from pathlib import Path
from PIL import Image

user32 = ctypes.WinDLL("user32", use_last_error=True)

class Rect(ctypes.Structure):
    _fields_ = [(k, wintypes.LONG) for k in ("left", "top", "right", "bottom")]

callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
user32.EnumWindows.argtypes = [callback_type, wintypes.LPARAM]
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
user32.IsWindowVisible.argtypes = [wintypes.HWND]
user32.GetWindow.argtypes = [wintypes.HWND, wintypes.UINT]
user32.GetWindow.restype = wintypes.HWND
user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(Rect)]
user32.GetClientRect.argtypes = [wintypes.HWND, ctypes.POINTER(Rect)]
user32.GetDpiForWindow.argtypes = [wintypes.HWND]
user32.GetDpiForWindow.restype = wintypes.UINT
user32.GetWindowLongPtrW.argtypes = [wintypes.HWND, ctypes.c_int]
user32.GetWindowLongPtrW.restype = ctypes.c_ssize_t
user32.AdjustWindowRectExForDpi.argtypes = [ctypes.POINTER(Rect), wintypes.DWORD,
    wintypes.BOOL, wintypes.DWORD, wintypes.UINT]
user32.SetWindowPos.argtypes = [wintypes.HWND, wintypes.HWND, ctypes.c_int, ctypes.c_int,
    ctypes.c_int, ctypes.c_int, wintypes.UINT]
user32.SetThreadDpiAwarenessContext.argtypes = [ctypes.c_void_p]
user32.SetThreadDpiAwarenessContext.restype = ctypes.c_void_p

def physical_pixel_context():
    assert user32.SetThreadDpiAwarenessContext(ctypes.c_void_p(-4))

def owned_window(pid):
    matches = []
    @callback_type
    def visit(hwnd, unused):
        candidate = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(candidate))
        if candidate.value == pid and user32.IsWindowVisible(hwnd) and not user32.GetWindow(hwnd, 4):
            client = Rect()
            if user32.GetClientRect(hwnd, ctypes.byref(client)) and client.right>client.left and client.bottom>client.top:
                matches.append(hwnd)
        return True
    assert user32.EnumWindows(visit, 0)
    if not matches: return None
    assert len(matches) == 1, "Expected one visible top-level window for the owned GUI PID"
    return matches[0]

def geometry(hwnd):
    window, client = Rect(), Rect()
    assert user32.GetWindowRect(hwnd, ctypes.byref(window))
    assert user32.GetClientRect(hwnd, ctypes.byref(client))
    dpi = user32.GetDpiForWindow(hwnd)
    assert dpi
    pixels = [client.right-client.left, client.bottom-client.top]
    return {"hwnd": int(hwnd), "dpi": dpi, "client_pixels": pixels,
        "client_logical": [p*96/dpi for p in pixels],
        "window_rect": [window.left, window.top, window.right, window.bottom]}

def resize_owned_client(hwnd, logical_size):
    dpi = user32.GetDpiForWindow(hwnd)
    rect = Rect(0, 0, round(logical_size[0]*dpi/96), round(logical_size[1]*dpi/96))
    style = user32.GetWindowLongPtrW(hwnd, -16) & 0xffffffff
    extended = user32.GetWindowLongPtrW(hwnd, -20) & 0xffffffff
    assert user32.AdjustWindowRectExForDpi(ctypes.byref(rect), style, False, extended, dpi)
    assert user32.SetWindowPos(hwnd, None, 0, 0, rect.right-rect.left, rect.bottom-rect.top,
        0x02 | 0x04 | 0x10)

def copy_owned_product_frame(source, destination, hwnd, after=0):
    source, destination = Path(source), Path(destination)
    try:
        before = source.stat()
        if before.st_mtime <= after: return None
        data = source.read_bytes()
        finished = source.stat()
        if (before.st_size, before.st_mtime_ns) != (finished.st_size, finished.st_mtime_ns): return None
        current = geometry(hwnd)
        with Image.open(io.BytesIO(data)) as image:
            image.load()
            if list(image.size) != current["client_pixels"]: return None
        destination.write_bytes(data)
    except (OSError, ValueError):
        return None  # Screenshot writes are not atomic; the caller has a finite retry deadline.
    return {"path": str(destination), "sha256": hashlib.sha256(data).hexdigest(),
        "geometry": current}
