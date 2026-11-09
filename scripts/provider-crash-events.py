"""Hosted qualification only: existing Event ID 1000, no target control/dumps."""
import ctypes
from ctypes import wintypes
from datetime import datetime, timezone
import ntpath
import re
import struct
import time
import xml.etree.ElementTree as ET


MAX_EVENTS = 32
MAX_XML_BYTES = 128 * 1024
NS = "{http://schemas.microsoft.com/win/2004/08/events/event}"
OWN_MODULES = {"protoinputmultiplex.exe":"product", "protoinputhooks64.dll":"protoinput_hooks",
    "protoinputloader64.dll":"protoinput_loader", "easyhook64.dll":"easyhook",
    "protoinputmultiplex_cursor_observer64.dll":"cursor_observer"}
SYSTEM_MODULES = frozenset({"ntdll.dll","kernelbase.dll","kernel32.dll","user32.dll","win32u.dll",
    "ucrtbase.dll","vcruntime140.dll","vcruntime140_1.dll","msvcp140.dll"})
FATAL_ENV = "PROTOINPUTMULTIPLEX_QUALIFICATION_FATAL_RECORD"
FATAL_MAGIC = 0x4c415441464d4950
FATAL_SIZE = 1152
FATAL_TOTAL = FATAL_SIZE * 2


def mapped_admissions(modules, end):
    # Only fixed admitted drives plus the fixed system-module directory. No PID
    # query, process reopen, module load or symbol lookup is performed here.
    if time.monotonic() >= end:
        raise TimeoutError()
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.QueryDosDeviceW.argtypes = [wintypes.LPCWSTR,wintypes.LPWSTR,wintypes.DWORD]
    kernel.QueryDosDeviceW.restype = wintypes.DWORD
    paths = {windows_path(path):sha for path, sha in modules.items()}
    drives = sorted({ntpath.splitdrive(path)[0] for path in paths} | {"c:"})
    devices = {}
    for drive in drives:
        if re.fullmatch(r"[a-z]:", drive) is None:
            continue
        buffer = ctypes.create_unicode_buffer(32768)
        if time.monotonic() >= end:
            raise TimeoutError()
        length = kernel.QueryDosDeviceW(drive, buffer, len(buffer))
        if 0 < length < len(buffer) and buffer.value.lower().startswith("\\device\\"):
            devices[drive] = buffer.value.lower()
    mapped = {devices[path[:2]]+path[2:]:sha for path, sha in paths.items() if path[:2] in devices}
    system = devices.get("c:")
    return mapped, None if system is None else system+r"\windows\system32"


def empty_fatal(status="unobserved"):
    return {"status":status,"module_class":None,"own_module_basename":None,
        "admitted_module_sha256":None,"instruction_rva":None,"exception_class":None,
        "filter_outcome":None,"loaded_module_hash_proved":False}


def fatal_record(raw, identity, exit_class, modules, system):
    public = empty_fatal()
    if identity is None:
        return empty_fatal("identity_unavailable")
    if exit_class != "access_violation":
        return empty_fatal("exit_unconfirmed")
    if len(raw) not in (FATAL_SIZE,FATAL_TOTAL):
        return empty_fatal("incomplete_or_invalid_size")
    setup = struct.unpack_from("<16Q", raw)
    if (setup[:3] != (FATAL_MAGIC,1,FATAL_SIZE) or setup[3:5] != identity
            or not 0 < setup[3] <= 0xffffffff or setup[4] == 0 or setup[5] != 1
            or setup[6] > 0xffffffff or any(setup[7:15]) or setup[15] != FATAL_MAGIC
            or any(raw[128:FATAL_SIZE])):
        return empty_fatal("invalid_setup_or_identity")
    if len(raw) == FATAL_SIZE:
        return empty_fatal("prepared_no_fatal_record")
    record = struct.unpack_from("<16Q", raw, FATAL_SIZE)
    if (record[:5] != setup[:5] or record[5] != 2 or record[6] != setup[6]
            or record[7] not in (0,1) or record[15] != FATAL_MAGIC):
        return empty_fatal("invalid_record_or_identity")
    if record[8] != 0xc0000005 or record[9] not in (0,1):
        return empty_fatal("exception_or_nonresume_unconfirmed")
    address, base, kind, length, path_status = record[10:15]
    if kind != 0x1000000 or base == 0 or address < base or address-base > 0xffffffff:
        return empty_fatal("image_relative_instruction_unavailable")
    if path_status != 1 or not 0 < length < 512:
        return empty_fatal("mapped_path_unavailable")
    path_bytes = raw[FATAL_SIZE+128:]
    if any(path_bytes[length*2:]):
        return empty_fatal("invalid_path_padding")
    try:
        path = path_bytes[:length*2].decode("utf-16-le").lower()
    except UnicodeError:
        return empty_fatal("invalid_mapped_path")
    if not path.startswith("\\device\\") or "\0" in path or ntpath.normpath(path) != path:
        return empty_fatal("invalid_mapped_path")
    basename = ntpath.basename(path)
    admitted = modules.get(path)
    if basename in OWN_MODULES and isinstance(admitted, str) and re.fullmatch(r"[0-9a-f]{64}", admitted):
        public.update(module_class=OWN_MODULES[basename],own_module_basename=basename,admitted_module_sha256=admitted)
    elif system is not None and ntpath.dirname(path) == system and basename in SYSTEM_MODULES:
        public["module_class"] = "windows_system"
    else:
        return empty_fatal("module_unrecognized")
    public.update(status="observed",instruction_rva=address-base,exception_class="access_violation",
        filter_outcome="continue_search" if record[9] == 0 else "execute_handler")
    return public


def windows_path(value):
    if not isinstance(value, str) or not value or "\0" in value or not ntpath.isabs(value):
        raise ValueError()
    return ntpath.normcase(ntpath.normpath(value.removeprefix("\\\\?\\")))


def original_creation(child, end):
    # Popen retains its original process object even after poll observes exit.
    if time.monotonic() >= end:
        raise TimeoutError()
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    kernel.GetProcessTimes.restype = wintypes.BOOL
    fields = [wintypes.FILETIME() for _ in range(4)]
    arguments = (wintypes.HANDLE(int(child._handle)), *(ctypes.byref(field) for field in fields))
    if time.monotonic() >= end:
        raise TimeoutError()
    if not kernel.GetProcessTimes(*arguments):
        raise OSError()
    creation = (fields[0].dwHighDateTime << 32) | fields[0].dwLowDateTime
    if type(child.pid) is not int or not 0 < child.pid <= 0xffffffff or creation == 0:
        raise ValueError()
    return child.pid, creation


def query_events(end):
    result = {"query_status":"not_started","record_limit_reached":False,"render_incomplete":False}
    records = []
    if time.monotonic() >= end:
        result["query_status"] = "deadline_exhausted"
        return result, records
    try:
        api = ctypes.WinDLL("wevtapi", use_last_error=True)
        api.EvtQuery.argtypes = [wintypes.HANDLE,wintypes.LPCWSTR,wintypes.LPCWSTR,wintypes.DWORD]
        api.EvtQuery.restype = wintypes.HANDLE
        api.EvtNext.argtypes = [wintypes.HANDLE,wintypes.DWORD,ctypes.POINTER(wintypes.HANDLE),
            wintypes.DWORD,wintypes.DWORD,ctypes.POINTER(wintypes.DWORD)]
        api.EvtNext.restype = wintypes.BOOL
        api.EvtRender.argtypes = [wintypes.HANDLE,wintypes.HANDLE,wintypes.DWORD,wintypes.DWORD,
            ctypes.c_void_p,ctypes.POINTER(wintypes.DWORD),ctypes.POINTER(wintypes.DWORD)]
        api.EvtRender.restype = wintypes.BOOL
        api.EvtClose.argtypes = [wintypes.HANDLE]
        api.EvtClose.restype = wintypes.BOOL
    except (AttributeError, OSError):
        result["query_status"] = "api_unavailable"
        return result, records
    # EvtQueryChannelPath|EvtQueryReverseDirection. One batch; never poll for a
    # future WER event. EvtNext timeout0 does not wait for a later record.
    handles = (wintypes.HANDLE * MAX_EVENTS)()
    count = wintypes.DWORD()
    if time.monotonic() >= end:
        result["query_status"] = "deadline_exhausted"
        return result, records
    query = api.EvtQuery(None, "Application", "*[System[Provider[@Name='Application Error'] and EventID=1000]]", 0x201)
    if not query:
        result["query_status"] = "query_failed"
        return result, records
    try:
        if time.monotonic() >= end:
            result["query_status"] = "deadline_exhausted"
            return result, records
        if not api.EvtNext(query, MAX_EVENTS, handles, 0, 0, ctypes.byref(count)):
            error = ctypes.get_last_error()
            result["query_status"] = {259:"no_records",1460:"query_timeout"}.get(error, "query_failed")
            return result, records
        if count.value > MAX_EVENTS:
            result["query_status"] = "query_failed"
            return result, records
        result["query_status"] = "queried"
        result["record_limit_reached"] = count.value == MAX_EVENTS
        for handle in handles[:count.value]:
            used, properties = wintypes.DWORD(), wintypes.DWORD()
            # Standard sizing call, then one bounded render; no retry on change.
            if time.monotonic() >= end:
                result["query_status"] = "deadline_exhausted"
                result["render_incomplete"] = True
                break
            sized = api.EvtRender(None, handle, 1, 0, None, ctypes.byref(used), ctypes.byref(properties))
            error = ctypes.get_last_error()
            if sized or error != 122 or not 2 <= used.value <= MAX_XML_BYTES or used.value % 2:
                result["render_incomplete"] = True
                continue
            buffer = ctypes.create_unicode_buffer(used.value // 2)
            if time.monotonic() >= end:
                result["query_status"] = "deadline_exhausted"
                result["render_incomplete"] = True
                break
            if not api.EvtRender(None, handle, 1, ctypes.sizeof(buffer), buffer, ctypes.byref(used), ctypes.byref(properties)):
                result["render_incomplete"] = True
                continue
            if not 2 <= used.value <= ctypes.sizeof(buffer):
                result["render_incomplete"] = True
                continue
            records.append(buffer.value)
    finally:
        for handle in handles:
            if handle:
                api.EvtClose(handle)
        api.EvtClose(query)
    return result, records


def system_filetime(value):
    match = re.fullmatch(r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d{1,7}))?Z", value or "")
    if match is None:
        raise ValueError()
    stamp = datetime.strptime(match[1], "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc)
    delta = stamp - datetime(1601, 1, 1, tzinfo=timezone.utc)
    return (delta.days * 86400 + delta.seconds) * 10000000 + int((match[2] or "").ljust(7, "0"))


def unsigned(value, bits, hexadecimal=False):
    if hexadecimal:
        if not re.fullmatch(r"(?:0[xX])?[0-9a-fA-F]{1,16}", value or ""):
            raise ValueError()
        number = int(value, 16)
    else:
        if not re.fullmatch(r"(?:0[xX][0-9a-fA-F]{1,16}|[0-9]{1,20})", value or ""):
            raise ValueError()
        number = int(value, 16 if value.lower().startswith("0x") else 10)
    if not 0 <= number < 1 << bits:
        raise ValueError()
    return number


def event_record(raw):
    if not isinstance(raw, str) or len(raw.encode("utf-16-le")) > MAX_XML_BYTES:
        raise ValueError()
    if "<!DOCTYPE" in raw.upper() or "<!ENTITY" in raw.upper():
        raise ValueError()
    root = ET.fromstring(raw)
    system = root.find(NS+"System")
    if root.tag != NS+"Event" or system is None or len(root.findall(NS+"System")) != 1:
        raise ValueError()
    if any(len(system.findall(NS+key)) != 1 for key in ("Provider","EventID","TimeCreated","Level","Channel")):
        raise ValueError()
    provider = system.find(NS+"Provider")
    if provider is None or provider.get("Name") != "Application Error" or system.findtext(NS+"EventID") != "1000":
        raise ValueError()
    if system.findtext(NS+"Level") != "2" or system.findtext(NS+"Channel") != "Application":
        raise ValueError()
    clock = system.find(NS+"TimeCreated")
    if clock is None:
        raise ValueError()
    data = root.find(NS+"EventData")
    if data is None or len(root.findall(NS+"EventData")) != 1 or len(data) > 32:
        raise ValueError()
    values = {}
    for node in data:
        key = node.get("Name")
        if node.tag != NS+"Data" or key is None or key in values or len(node):
            raise ValueError()
        values[key] = node.text or ""
    row = {"pid":unsigned(values.get("ProcessId"),32),
        "creation":unsigned(values.get("ProcessCreationTime"),64),
        "time":system_filetime(clock.get("SystemTime")),
        "image":windows_path(values.get("AppPath")),
        "module_name":values.get("ModuleName", "").lower()}
    for key, field, parse in (("module","ModulePath",windows_path),
            ("exception","ExceptionCode",lambda value: unsigned(value,32,True)),
            ("rva","FaultingOffset",lambda value: unsigned(value,32,True))):
        try:
            row[key] = parse(values.get(field))
        except (ValueError, TypeError):
            row[key] = None
    return row


def empty_observation(status="unobserved"):
    return {"status":status,"attribution_status":None,"module_class":None,"own_module_basename":None,
        "admitted_module_sha256":None,"instruction_rva":None,"exception_class":None,
        "symbolization":"not_inspected","record_retention_failed":False}


def match_events(records, identity, image, upper, modules):
    matches, malformed = [], False
    for raw in records:
        try:
            row = event_record(raw)
        except (ValueError, TypeError, ET.ParseError, UnicodeError, OverflowError):
            malformed = True
            continue
        if ((row["pid"],row["creation"]) == identity and row["image"] == windows_path(image)
                and identity[1] <= row["time"] <= upper):
            matches.append((row,raw))
    if len(matches) != 1:
        return empty_observation("ambiguous" if matches else "no_matching_event"), None, malformed
    row, raw = matches[0]
    public = empty_observation("matched")
    public["attribution_status"] = "available"
    for key, reason in (("module","module_unavailable"),("rva","offset_unavailable"),("exception","exception_unavailable")):
        if row[key] is None:
            public.update(status="matched_unattributable",attribution_status=reason)
            return public, raw, malformed
    public["instruction_rva"] = row["rva"]
    public["exception_class"] = "access_violation" if row["exception"] == 0xc0000005 else "other_exception"
    public["module_class"] = "other_module"
    basename = ntpath.basename(row["module"])
    admitted = modules.get(row["module"])
    if (basename == row["module_name"] and basename in OWN_MODULES and isinstance(admitted, str)
            and re.fullmatch(r"[0-9a-f]{64}", admitted) is not None):
        public.update(module_class=OWN_MODULES[basename],own_module_basename=basename,admitted_module_sha256=admitted)
    elif (basename == row["module_name"] and basename in SYSTEM_MODULES
            and ntpath.dirname(row["module"]) == r"c:\windows\system32"):
        public["module_class"] = "windows_system"
    return public, raw, malformed
