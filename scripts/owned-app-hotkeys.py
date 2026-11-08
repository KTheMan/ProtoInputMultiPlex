"""Own-window shortcut fixture for the independent lifecycle acceptance gate.

Do not import/run until the root grants the exclusive native slot. Registration
uses a private message-only STATIC window on its owning thread. No physical key
is pressed, polled, or injected. All registrations are retired before teardown.
"""
import ctypes
from ctypes import wintypes
import queue
import threading
import time

user32 = ctypes.WinDLL("user32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
HWND_MESSAGE = ctypes.c_void_p(-3)
LIFECYCLE_CLASS = "ProtoInputMultiPlex.AppLifecycle.v1"
MODIFIERS = 0x0001 | 0x0002 | 0x0004 | 0x4000
user32.CreateWindowExW.argtypes = [wintypes.DWORD, wintypes.LPCWSTR, wintypes.LPCWSTR,
    wintypes.DWORD, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int,
    wintypes.HWND, wintypes.HMENU, wintypes.HINSTANCE, ctypes.c_void_p]
user32.CreateWindowExW.restype = wintypes.HWND
user32.FindWindowExW.argtypes = [wintypes.HWND, wintypes.HWND, wintypes.LPCWSTR, wintypes.LPCWSTR]
user32.FindWindowExW.restype = wintypes.HWND
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
user32.GetWindowThreadProcessId.restype = wintypes.DWORD
user32.RegisterHotKey.argtypes = [wintypes.HWND, ctypes.c_int, wintypes.UINT, wintypes.UINT]
user32.UnregisterHotKey.argtypes = [wintypes.HWND, ctypes.c_int]
user32.DestroyWindow.argtypes = [wintypes.HWND]
user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, ctypes.c_size_t, ctypes.c_ssize_t]
user32.PeekMessageW.argtypes = [ctypes.POINTER(wintypes.MSG), wintypes.HWND, wintypes.UINT, wintypes.UINT, wintypes.UINT]
user32.TranslateMessage.argtypes = [ctypes.POINTER(wintypes.MSG)]
user32.DispatchMessageW.argtypes = [ctypes.POINTER(wintypes.MSG)]
user32.DispatchMessageW.restype = ctypes.c_ssize_t
kernel32.GetModuleHandleW.argtypes = [wintypes.LPCWSTR]
kernel32.GetModuleHandleW.restype = wintypes.HMODULE

def lifecycle_window(pid):
    """Enumerate only this product's hidden top-level class and exact PID."""
    found = []
    previous = None
    while True:
        hwnd = user32.FindWindowExW(None, previous, LIFECYCLE_CLASS, None)
        if not hwnd: break
        actual = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(actual))
        if actual.value == pid: found.append(int(hwnd))
        previous = hwnd
    assert len(found) <= 1, "Multiple lifecycle message windows for owned GUI"
    return found[0] if found else None

def typed_hotkey(pid, hwnd, identifier):
    actual = wintypes.DWORD()
    user32.GetWindowThreadProcessId(hwnd, ctypes.byref(actual))
    assert actual.value == pid and hwnd == lifecycle_window(pid)
    assert user32.PostMessageW(hwnd, 0x0312, identifier, 0)
    return {"method":"owned_PostMessage_WM_HOTKEY", "pid":pid, "hwnd":hwnd, "id":identifier}

class OwnedHotkeys:
    def __init__(self):
        self.requests = queue.Queue()
        self.ready = queue.Queue()
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()
        self.hwnd = self.ready.get(timeout=4)
        assert self.hwnd, "Could not create the own hotkey-conflict window"
        self.next_id = 0x6800

    def _run(self):
        hwnd = user32.CreateWindowExW(0, "STATIC", "Owned lifecycle conflict fixture", 0,
            0, 0, 0, 0, HWND_MESSAGE, None, kernel32.GetModuleHandleW(None), None)
        self.ready.put(int(hwnd) if hwnd else 0)
        if not hwnd: return
        registrations = set()
        try:
            while True:
                msg = wintypes.MSG()
                while user32.PeekMessageW(ctypes.byref(msg), None, 0, 0, 1):
                    user32.TranslateMessage(ctypes.byref(msg))
                    user32.DispatchMessageW(ctypes.byref(msg))
                try: operation, identifier, vk, modifiers, reply = self.requests.get(timeout=.01)
                except queue.Empty: continue
                if operation == "close":
                    reply.put(True)
                    break
                if operation == "register":
                    ctypes.set_last_error(0)
                    accepted = bool(user32.RegisterHotKey(hwnd, identifier, modifiers, vk))
                    error = ctypes.get_last_error()
                    if accepted: registrations.add(identifier)
                    reply.put({"accepted":accepted, "error":error, "id":identifier,
                               "vk":vk, "modifiers":modifiers, "hwnd":int(hwnd)})
                elif operation == "unregister":
                    accepted = bool(user32.UnregisterHotKey(hwnd, identifier))
                    if accepted: registrations.discard(identifier)
                    reply.put(accepted)
        finally:
            for identifier in registrations: user32.UnregisterHotKey(hwnd, identifier)
            user32.DestroyWindow(hwnd)

    def register(self, vk, modifiers=MODIFIERS):
        self.next_id += 1
        reply = queue.Queue()
        self.requests.put(("register", self.next_id, vk, modifiers, reply))
        return reply.get(timeout=3)

    def unregister(self, identifier):
        reply = queue.Queue()
        self.requests.put(("unregister", identifier, 0, 0, reply))
        assert reply.get(timeout=3), "Owned registration did not unregister"

    def probe(self, vk, expected, modifiers=MODIFIERS):
        result = self.register(vk, modifiers)
        try: assert result["accepted"] is expected, result
        finally:
            if result["accepted"]: self.unregister(result["id"])
        return result

    def close(self):
        if self.thread.is_alive():
            reply = queue.Queue()
            self.requests.put(("close", 0, 0, 0, reply))
            assert reply.get(timeout=3)
            self.thread.join(timeout=3)
            assert not self.thread.is_alive(), "Owned conflict thread did not finish"

    def __enter__(self): return self
    def __exit__(self, *unused): self.close()
