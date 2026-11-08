"""Acceptance helper for a real popup owned by one explicitly supplied app PID.

Prepare/AST-check before READY; call only in the exclusive native test slot.
Opens the product's actual notification callback, inspects its Win32 HMENU,
and invokes the real MenuItem accessibility action. Never sends WM_COMMAND,
moves a pointer, captures the desktop, or searches unrelated accessibility trees.
"""
import ctypes
from ctypes import wintypes
import time

user32 = ctypes.WinDLL("user32", use_last_error=True)
shell32 = ctypes.WinDLL("shell32", use_last_error=True)

class Rect(ctypes.Structure):
    _fields_ = [(k,wintypes.LONG)for k in ("left","top","right","bottom")]

class MenuBarInfo(ctypes.Structure):
    _fields_ = [("size",wintypes.DWORD),("rect",Rect),("menu",wintypes.HMENU),
                ("window",wintypes.HWND),("flags",wintypes.DWORD)]

class MenuItemInfo(ctypes.Structure):
    _fields_ = [("size",wintypes.UINT),("mask",wintypes.UINT),("type",wintypes.UINT),
                ("state",wintypes.UINT),("id",wintypes.UINT),("submenu",wintypes.HMENU),
                ("checked",wintypes.HBITMAP),("unchecked",wintypes.HBITMAP),
                ("data",ctypes.c_size_t),("text",wintypes.LPWSTR),("count",wintypes.UINT),
                ("bitmap",wintypes.HBITMAP)]

class Guid(ctypes.Structure):
    _fields_ = [("a",wintypes.DWORD),("b",wintypes.WORD),("c",wintypes.WORD),
                ("d",ctypes.c_ubyte*8)]

class NotifyIdentifier(ctypes.Structure):
    _fields_ = [("size",wintypes.DWORD),("window",wintypes.HWND),("id",wintypes.UINT),("guid",Guid)]

callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL,wintypes.HWND,wintypes.LPARAM)
user32.EnumThreadWindows.argtypes = [wintypes.DWORD,callback_type,wintypes.LPARAM]
user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND,ctypes.POINTER(wintypes.DWORD)]
user32.GetWindowThreadProcessId.restype = wintypes.DWORD
user32.GetClassNameW.argtypes = [wintypes.HWND,wintypes.LPWSTR,ctypes.c_int]
user32.IsWindowVisible.argtypes = [wintypes.HWND]
user32.IsWindow.argtypes = [wintypes.HWND]
user32.GetMenuBarInfo.argtypes = [wintypes.HWND,wintypes.LONG,wintypes.LONG,ctypes.POINTER(MenuBarInfo)]
user32.GetMenuItemCount.argtypes = [wintypes.HMENU]
user32.GetMenuItemInfoW.argtypes = [wintypes.HMENU,wintypes.UINT,wintypes.BOOL,ctypes.POINTER(MenuItemInfo)]
user32.PostMessageW.argtypes = [wintypes.HWND,wintypes.UINT,ctypes.c_size_t,ctypes.c_ssize_t]
shell32.Shell_NotifyIconGetRect.argtypes = [ctypes.POINTER(NotifyIdentifier),ctypes.POINTER(Rect)]
shell32.Shell_NotifyIconGetRect.restype = ctypes.c_long

def owner_thread(hwnd,pid):
    observed=wintypes.DWORD()
    thread=user32.GetWindowThreadProcessId(hwnd,ctypes.byref(observed))
    assert thread and observed.value==pid,"Native HWND is not owned by the declared disposable app PID"
    return thread

def icon_rectangle(pid,window,icon_id):
    owner_thread(window,pid)
    identifier=NotifyIdentifier()
    identifier.size=ctypes.sizeof(identifier);identifier.window=window;identifier.id=icon_id
    rect=Rect()
    result=shell32.Shell_NotifyIconGetRect(ctypes.byref(identifier),ctypes.byref(rect))
    return {"hresult":result,"rectangle":[rect.left,rect.top,rect.right,rect.bottom] if result==0 else None}

def visible_popup(pid,thread):
    matches=[]
    @callback_type
    def visit(hwnd,unused):
        observed=wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd,ctypes.byref(observed))
        if observed.value!=pid or not user32.IsWindowVisible(hwnd):return True
        name=ctypes.create_unicode_buffer(64)
        if user32.GetClassNameW(hwnd,name,len(name)) and name.value=="#32768":matches.append(hwnd)
        return True
    assert user32.EnumThreadWindows(thread,visit,0)
    if not matches:return None
    assert len(matches)==1,"Ambiguous native popup on the owned lifecycle thread"
    info=MenuBarInfo();info.size=ctypes.sizeof(info)
    # Visibility can precede completion of the standard menu's initialization.
    # The caller's finite deadline still requires a genuine usable HMENU.
    if not user32.GetMenuBarInfo(matches[0],-4,0,ctypes.byref(info))or not info.menu:return None
    if user32.GetMenuItemCount(info.menu)<=0:return None
    return matches[0],info

def menu_items(menu):
    count=user32.GetMenuItemCount(menu)
    assert 0<count<=32,"Unexpected owned tray menu item count"
    result=[]
    for index in range(count):
        text=ctypes.create_unicode_buffer(512)
        item=MenuItemInfo();item.size=ctypes.sizeof(item);item.mask=0x40|0x100|0x01|0x02
        item.text=ctypes.cast(text,wintypes.LPWSTR);item.count=len(text)
        assert user32.GetMenuItemInfoW(menu,index,True,ctypes.byref(item))
        result.append({"index":index,"id":item.id,"name":text.value,"enabled":not bool(item.state&3),
                       "separator":bool(item.type&0x800)})
    return result

def open_menu(pid,window,callback_message,wparam,lparam,timeout=4):
    thread=owner_thread(window,pid)
    assert user32.PostMessageW(window,callback_message,wparam,lparam)
    deadline=time.monotonic()+timeout
    while time.monotonic()<deadline:
        found=visible_popup(pid,thread)
        if found:
            hwnd,info=found
            return {"pid":pid,"thread":thread,"window":int(hwnd),"menu":int(info.menu),
                    "bounds":[info.rect.left,info.rect.top,info.rect.right,info.rect.bottom],
                    "items":menu_items(info.menu)}
        time.sleep(.025)
    raise AssertionError("Real owned native tray popup did not become visible")

def normalized_name(value):
    return value.replace("&","").split("\t",1)[0].strip()

def invoke_item(snapshot,label):
    """Activate the actual native MSAA MenuItem; caller verifies action effects.

    ABI slots and IID follow the installed Windows SDK oleacc.h IAccessibleVtbl.
    Use the system oleacc provider directly: the environment's comtypes namespace
    has no implementation. No command ID is sent or substituted for selection.
    """
    owner_thread(snapshot["window"],snapshot["pid"])
    assert user32.IsWindowVisible(snapshot["window"])
    native=[i for i in snapshot["items"]if not i["separator"]and normalized_name(i["name"])==label]
    assert len(native)==1 and native[0]["enabled"],(label,snapshot["items"])
    class Record(ctypes.Structure):
        _fields_=[("data",ctypes.c_void_p),("information",ctypes.c_void_p)]
    class Value(ctypes.Union):
        _fields_=[("integer",wintypes.LONG),("wide",ctypes.c_ulonglong),("pointer",ctypes.c_void_p),("record",Record)]
    class Variant(ctypes.Structure):
        _anonymous_=("value",)
        _fields_=[("vt",wintypes.WORD),("reserved1",wintypes.WORD),("reserved2",wintypes.WORD),
                  ("reserved3",wintypes.WORD),("value",Value)]
    assert ctypes.sizeof(Variant)==(24 if ctypes.sizeof(ctypes.c_void_p)==8 else 16)
    ole32=ctypes.WinDLL("ole32");oleacc=ctypes.WinDLL("oleacc");oleaut=ctypes.WinDLL("oleaut32")
    ole32.CoInitializeEx.argtypes=[ctypes.c_void_p,wintypes.DWORD];ole32.CoInitializeEx.restype=ctypes.c_long
    ole32.CoUninitialize.argtypes=[]
    oleacc.AccessibleObjectFromWindow.argtypes=[wintypes.HWND,wintypes.DWORD,ctypes.POINTER(Guid),ctypes.POINTER(ctypes.c_void_p)]
    oleacc.AccessibleObjectFromWindow.restype=ctypes.c_long
    oleaut.SysFreeString.argtypes=[ctypes.c_void_p]
    oleaut.VariantClear.argtypes=[ctypes.POINTER(Variant)]
    initialized=ole32.CoInitializeEx(None,2)
    assert initialized in (0,1,-2147417850),initialized # existing apartment is also usable
    iid=Guid(0x618736E0,0x3C3D,0x11CF,(ctypes.c_ubyte*8)(0x81,0x0c,0,0xaa,0,0x38,0x9b,0x71))
    accessible=ctypes.c_void_p()
    try:
        result=oleacc.AccessibleObjectFromWindow(snapshot["window"],0xFFFFFFFC,ctypes.byref(iid),ctypes.byref(accessible))
        assert result>=0 and accessible.value,("Actual own popup accessibility",result)
        table=ctypes.cast(accessible,ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))).contents
        def method(index,*types):return ctypes.WINFUNCTYPE(ctypes.c_long,ctypes.c_void_p,*types)(table[index])
        def child_variant(identifier):
            child=Variant();child.vt=3;child.integer=identifier;return child
        def text(index,identifier):
            value=ctypes.c_void_p()
            result=method(index,Variant,ctypes.POINTER(ctypes.c_void_p))(accessible,child_variant(identifier),ctypes.byref(value))
            assert result>=0,(index,identifier,result)
            try:return ctypes.wstring_at(value.value)if value.value else""
            finally:
                if value.value:oleaut.SysFreeString(value)
        count=wintypes.LONG()
        assert method(8,ctypes.POINTER(wintypes.LONG))(accessible,ctypes.byref(count))>=0
        assert count.value==len(snapshot["items"]),(count.value,snapshot["items"])
        observed=[];chosen=[]
        for identifier in range(1,count.value+1):
            name=text(10,identifier);state=Variant();role=Variant()
            assert method(14,Variant,ctypes.POINTER(Variant))(accessible,child_variant(identifier),ctypes.byref(state))>=0
            assert method(13,Variant,ctypes.POINTER(Variant))(accessible,child_variant(identifier),ctypes.byref(role))>=0
            assert state.vt==3 and role.vt==3
            item={"child_id":identifier,"name":name,"role":role.integer,"state":state.integer,
                  "default_action":text(20,identifier)}
            oleaut.VariantClear(ctypes.byref(state));oleaut.VariantClear(ctypes.byref(role))
            observed.append(item)
            if normalized_name(name)==label:chosen.append(item)
        assert len(chosen)==1,(label,observed)
        selected=chosen[0]
        assert selected["role"]==12 and not selected["state"]&(1|0x8000|0x10000),selected
        assert selected["default_action"],"Actual MenuItem has no default action"
        result=method(25,Variant)(accessible,child_variant(selected["child_id"]))
        assert result>=0,("Actual menu accDoDefaultAction",result)
        return {"method":"actual_native_menu_MSAA_accDoDefaultAction","label":label,"native_item":native[0],
                "accessibility_items":observed,"popup":snapshot}
    finally:
        if accessible.value:
            table=ctypes.cast(accessible,ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))).contents
            ctypes.WINFUNCTYPE(wintypes.ULONG,ctypes.c_void_p)(table[2])(accessible)
        if initialized in (0,1):ole32.CoUninitialize()
