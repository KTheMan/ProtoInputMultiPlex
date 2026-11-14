"""Opt-in hosted crash diagnostics. All inputs/tool output stay private.

This is fixture orchestration and offline analysis, never product behavior or
functional qualification. A retained dump is useful only after all four owned
identity records agree. No live process is opened, waited on, or debugged here.
"""
import argparse
import ctypes
import hashlib
import json
import ntpath
import os
from pathlib import Path
import re
import struct
import subprocess
import time


CONFIG_ENV = "PROTOINPUTMUX_PRIVATE_DUMP_CONFIG"
PREPARE_PHASES = frozenset({"not_started", "root", "metadata", "probe_identity", "probe_file_hashes",
    "probe_pe_codeview", "probe_pdb_identity", "probe_identity_match", "toolchain_validation",
    "helper_admission", "module_admission", "deadline_config", "ready"})
WAIT_MAGIC = 0x31544941574D4950
FATAL_MAGIC = 0x4C415441464D4950
MAX_DUMP = 64 * 1024 * 1024
MAX_BINARY = 512 * 1024 * 1024
MAX_TOOL = 2 * 1024 * 1024
KITS = r"C:\Program Files (x86)\Windows Kits\10\Debuggers\x64"
LLVM = r"C:\Program Files\LLVM\bin"
POWERSHELL = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
OWN = frozenset({"protoinputmultiplex.exe", "protoinputhooks64.dll", "protoinputloader64.dll",
    "easyhook64.dll", "protoinputmultiplex_cursor_observer64.dll"})
# These are exact metadata observations from the independently retained hosted
# inventory, not discovery hints. A different/absent version is inconclusive.
TOOLS = {
    "cdb.exe": (KITS, "e8f7d1e2986e541c82eb7917460e8118e7a262c16e402a5b1333b01f147a0fc3", "10.0.26100.9457", "Valid"),
    "dbghelp.dll": (KITS, "3ce12537291b46af6914ea6865f176d9528cd7707c752fca8be96ad2a7562220", "10.0.26100.9457", "Valid"),
    "dbgcore.dll": (KITS, "d63e8421d6847ac0d87a9bd151740610dd8a580c8ad67152d038f4c504d4b292", "10.0.26100.9457", "Valid"),
    "dbgeng.dll": (KITS, "f70a0ae7ebd1c37da334d6b3c388aba107b54b4de869bdbd54a270251145e649", "10.0.26100.9457", "Valid"),
    "symchk.exe": (KITS, "c24ea16f1d23bf5cbd32992fda34239b4e7af8c46bf084326a4bacdb8f27ea7a", "10.0.26100.9457", "Valid"),
    "dumpchk.exe": (KITS, "fa53a012a11c5fc9f2042cde8c586054ba63781a8bea182670f4b18199b78b76", "10.0.26100.9457", "Valid"),
    "llvm-pdbutil.exe": (LLVM, "2fd4e8ec4a57b5944a01a7ed31a0281c1d5d28a8b6aa142ca1c31d23b9bac26a", "20.1.8.0", "NotSigned"),
    "llvm-readobj.exe": (LLVM, "9c6b8adcaf1fea2889105e1d97f5b319b7d600f053069baa817ce462b611805a", "20.1.8.0", "NotSigned"),
    "llvm-symbolizer.exe": (LLVM, "b1401069b28de34cf0a284516e46a9f2617b50c5d0d4c8cb78598a2c1208aa81", "20.1.8.0", "NotSigned"),
    "llvm-objdump.exe": (LLVM, "dad81b151bde105292a07c2ccc763f18667715a8fd1f7f2f4c35c8440ec1ed83", "20.1.8.0", "NotSigned"),
}


class Rejected(ValueError):
    pass


class CaptureRejected(Rejected):
    def __init__(self, reason):
        self.reason = reason


def need(condition):
    if not condition:
        raise Rejected()


def integer(value, maximum=0xFFFFFFFFFFFFFFFF):
    need(type(value) is int and 0 <= value <= maximum)
    return value


def span(raw, offset, size):
    need(type(offset) is int and type(size) is int and 0 <= offset <= len(raw)
        and 0 <= size <= len(raw)-offset)
    return raw[offset:offset+size]


def unpack(fmt, raw, offset=0):
    return struct.unpack(fmt, span(raw, offset, struct.calcsize(fmt)))


def bounded(path, maximum):
    with Path(path).open("rb") as source:
        raw = source.read(maximum+1)
    need(len(raw) <= maximum)
    return raw


def digest(path):
    # Bound file reads even for a malformed local symbol specimen.
    return hashlib.sha256(bounded(path, MAX_BINARY)).hexdigest()


def strict_json(raw):
    def pairs(values):
        result = {}
        for key, value in values:
            need(key not in result)
            result[key] = value
        return result
    return json.loads(raw.decode("utf-8-sig"), object_pairs_hook=pairs,
        parse_constant=lambda _: need(False))


def local_path(value):
    need(type(value) is str and 0 < len(value) <= 1024 and "\0" not in value
        and re.match(r"^[A-Za-z]:[\\/]", value) is not None and not value.startswith("\\\\"))
    need(not any(c in value for c in '\r\n;"'))
    return ntpath.normcase(ntpath.normpath(value))


def pdb_path_matches(codeview_path, image_path, admitted_path):
    """Accept a relative CodeView basename while binding absolute paths exactly."""
    expected = local_path(admitted_path)
    image = local_path(image_path)
    if type(codeview_path) is not str or not 0 < len(codeview_path) <= 1024 or "\0" in codeview_path:
        return False
    if ntpath.splitdrive(codeview_path)[0] or codeview_path.startswith(("\\", "/")):
        try:
            return local_path(codeview_path) == expected
        except ValueError:
            return False
    return ("\\" not in codeview_path and "/" not in codeview_path and ":" not in codeview_path
        and ntpath.normcase(codeview_path) == ntpath.basename(expected)
        and ntpath.dirname(image) == ntpath.dirname(expected))


def rsds(raw):
    need(25 <= len(raw) <= 4096 and raw[:4] == b"RSDS" and b"\0" in raw[24:])
    age, = unpack("<I", raw, 20)
    need(age > 0)
    return raw[4:20], age


def pe(raw, require_codeview=True):
    """Minimal bounded AMD64 PE reader; no image loading or execution."""
    need(raw[:2] == b"MZ")
    offset, = unpack("<I", raw, 60)
    need(span(raw, offset, 4) == b"PE\0\0")
    machine, count, stamp, _, _, optional_size, _ = unpack("<HHIIIHH", raw, offset+4)
    need(machine == 0x8664 and 1 <= count <= 96 and optional_size >= 240)
    optional = offset+24
    need(unpack("<H", raw, optional)[0] == 0x20B)
    image_size, headers = unpack("<II", raw, optional+56)
    image_base, = unpack("<Q", raw, optional+24)
    need(image_size > 0 and headers <= len(raw))
    directories, = unpack("<I", raw, optional+108)
    need(7 <= directories <= 16 and optional_size >= 112+8*directories)
    sections = []
    for index in range(count):
        item = optional+optional_size+40*index
        virtual, address, size, file_offset = unpack("<IIII", raw, item+8)
        flags, = unpack("<I", raw, item+36)
        need(address+max(virtual, size) <= image_size and file_offset+size <= len(raw))
        sections.append((address, max(virtual, size), file_offset, size, flags))
    def rva(address, size):
        need(0 <= address < image_size)
        if address < headers:
            return span(raw, address, size)
        matches = [s for s in sections if s[0] <= address and size <= s[3]-(address-s[0])]
        need(len(matches) == 1)
        section = matches[0]
        return span(raw, section[2]+address-section[0], size)
    debug_rva, debug_size = unpack("<II", raw, optional+112+6*8)
    need(0 <= debug_size <= 28*64 and debug_size % 28 == 0)
    identities, pdb_paths = [], []
    debug = rva(debug_rva, debug_size) if debug_size else b""
    for index in range(debug_size//28):
        _, _, _, _, kind, size, address, file_offset = unpack("<IIHHIIII", debug, index*28)
        if kind == 2:
            need(rva(address, size) == span(raw, file_offset, size))
            codeview = span(raw, file_offset, size)
            identities.append(rsds(codeview))
            pdb_paths.append(codeview[24:codeview.index(b"\0", 24)].decode("utf-8"))
    need(len(identities) == 1 if require_codeview else len(identities) <= 1)
    exports = set()
    export_rva, export_size = unpack("<II", raw, optional+112)
    if export_rva and export_size:
        item = rva(export_rva, 40)
        names, = unpack("<I", item, 24)
        names_rva, = unpack("<I", item, 32)
        need(names <= 65536)
        for index in range(names):
            name_rva, = unpack("<I", rva(names_rva+4*index, 4))
            name = bytearray()
            for position in range(256):
                byte = rva(name_rva+position, 1)[0]
                if byte == 0:
                    break
                name.append(byte)
            else:
                raise Rejected()
            exports.add(bytes(name))
    return {"cv":identities[0] if identities else None, "pdb_path":pdb_paths[0] if pdb_paths else None, "size":image_size, "stamp":stamp, "base":image_base,
        "sections":sections, "exports":exports}


def pdb_identity(raw):
    """MSF 7 stream 1 GUID/age, with bounded block-directory validation."""
    need(raw[:32] == b"Microsoft C/C++ MSF 7.00\r\n\x1aDS\0\0\0")
    block, _, blocks, directory_size, reserved, block_map = unpack("<6I", raw, 32)
    need(block in (512, 1024, 2048, 4096, 8192) and blocks*block == len(raw)
        and reserved == 0 and 4 <= directory_size <= 16*1024*1024)
    count = (directory_size+block-1)//block
    need(count*4 <= block and block_map < blocks)
    indices = unpack("<"+"I"*count, raw, block_map*block)
    need(all(index < blocks for index in indices) and len(set(indices)) == len(indices))
    directory = b"".join(span(raw, index*block, block) for index in indices)[:directory_size]
    streams, = unpack("<I", directory)
    need(2 <= streams <= 65536)
    sizes = unpack("<"+"I"*streams, directory, 4)
    cursor = 4+4*streams
    info = None
    for index, size in enumerate(sizes):
        if size == 0xFFFFFFFF:
            continue
        need(size <= MAX_BINARY)
        number = (size+block-1)//block
        pages = unpack("<"+"I"*number, directory, cursor) if number else ()
        cursor += number*4
        need(all(page < blocks for page in pages) and len(set(pages)) == len(pages))
        if index == 1:
            need(28 <= size <= 16*1024*1024)
            info = b"".join(span(raw, page*block, block) for page in pages)[:size]
    need(cursor == len(directory) and info is not None)
    version, _, age = unpack("<III", info)
    need(version == 20000404 and age > 0)
    return span(info, 12, 16), age


def dump(raw, status):
    need(32 <= len(raw) <= MAX_DUMP and raw[:4] == b"MDMP")
    version, count, directory, _, _, flags = unpack("<IIIIIQ", raw, 4)
    need(version & 0xFFFF == 0xA793 and 1 <= count <= 64)
    # Reject full-memory, full-memory-info is metadata and intentionally allowed.
    need(flags & ~0x7820 == 0)
    streams = {}
    for index in range(count):
        kind, size, rva = unpack("<III", raw, directory+index*12)
        need(kind not in streams and rva >= 32)
        streams[kind] = span(raw, rva, size)
    need(all(kind in streams for kind in (3, 4, 6, 7, 17)))
    need(unpack("<H", streams[7])[0] == 9)  # PROCESSOR_ARCHITECTURE_AMD64
    exception = streams[6]
    need(len(exception) == 168)
    thread, padding = unpack("<II", exception)
    code, exception_flags, chain, address, parameters, unused = unpack("<IIQQII", exception, 8)
    need(padding == unused == chain == 0 and code == 0xC0000005 and parameters <= 15
        and thread == status["thread_id"] and address == status["exception_address"]
        and exception_flags == status["exception_flags"] and parameters == status["exception_parameter_count"])
    exception_parameters = list(unpack("<15Q", exception, 40))
    need(exception_parameters[:parameters] == status["exception_parameters"][:parameters]
        and not any(exception_parameters[parameters:]))
    size, rva = unpack("<II", exception, 160)
    need(256 <= size <= 4096)
    context = span(raw, rva, size)
    context_flags, = unpack("<I", context, 48)
    rsp, = unpack("<Q", context, 152)
    rip, = unpack("<Q", context, 248)
    need(context_flags == 0x100003 and rip == status["original_rip"] and rsp == status["original_rsp"]
        and list(unpack("<17Q", context, 120)) == status["integer_registers"])
    threads = streams[3]
    thread_count, = unpack("<I", threads)
    need(1 <= thread_count <= 4096 and len(threads) == 4+48*thread_count)
    matching = []
    for index in range(thread_count):
        row = span(threads, 4+48*index, 48)
        if unpack("<I", row)[0] == thread:
            matching.append(row)
    need(len(matching) == 1)
    thread_info = streams[17]
    header_size, entry_size, entry_count = unpack("<III", thread_info)
    need(header_size == 12 and entry_size == 64 and 1 <= entry_count <= 4096
        and len(thread_info) == header_size+entry_count*entry_size)
    matching_info = [span(thread_info, header_size+index*entry_size, entry_size) for index in range(entry_count)
        if unpack("<I", thread_info, header_size+index*entry_size)[0] == thread]
    need(len(matching_info) == 1 and unpack("<Q", matching_info[0], 16)[0] == status["thread_creation"])
    stack_base, stack_size, stack_rva = unpack("<QII", matching[0], 24)
    stack = span(raw, stack_rva, stack_size)
    need(0 < stack_size <= MAX_DUMP and stack_base <= rsp < stack_base+stack_size)
    modules = []
    rows = streams[4]
    module_count, = unpack("<I", rows)
    need(1 <= module_count <= 4096 and len(rows) == 4+108*module_count)
    for index in range(module_count):
        row = span(rows, 4+108*index, 108)
        base, size, _, stamp, name_rva = unpack("<QIIII", row)
        need(base > 0 and 0 < size <= 0xFFFFFFFF and base+size <= 0xFFFFFFFFFFFFFFFF)
        name_size, = unpack("<I", raw, name_rva)
        need(0 < name_size <= 2048 and name_size % 2 == 0)
        name = span(raw, name_rva+4, name_size).decode("utf-16-le")
        need("\0" not in name)
        cv_size, cv_rva = unpack("<II", row, 76)
        cv = rsds(span(raw, cv_rva, cv_size)) if cv_size else None
        need(not any(base < old["base"]+old["size"] and old["base"] < base+size for old in modules))
        modules.append({"base":base, "size":size, "stamp":stamp, "path":name, "cv":cv})
    # Candidates are private raw stack values, never caller proofs by themselves.
    candidates = []
    start = rsp-stack_base
    for offset in range(start, min(len(stack)-7, start+4096), 8):
        value, = unpack("<Q", stack, offset)
        if any(row["base"] <= value < row["base"]+row["size"] for row in modules):
            candidates.append(value)
    return {"rip":rip, "rsp":rsp, "modules":modules, "candidates":candidates[:32]}


STATUS_FIELDS = frozenset({"schema", "status", "pid", "creation", "thread_id", "exception_code",
    "filter_result", "original_rip", "original_rsp", "original_context_flags", "basic_context_only",
    "thread_owner_pid_verified", "dump_bytes", "one_attempt", "deadline_tick_ms", "source_commit",
    "thread_creation", "exception_address", "exception_flags", "exception_parameter_count",
    "exception_parameters", "integer_registers"})


def owned_records(fatal, status_raw, wait_raw, identity, exit_class, expected_deadline, source_commit):
    need(exit_class == "access_violation" and type(identity) is tuple and len(identity) == 2)
    need(integer(identity[0], 0xFFFFFFFF) > 0 and integer(identity[1]) > 0)
    need(len(fatal) == 2304)
    setup, record = unpack("<16Q", fatal), unpack("<16Q", fatal, 1152)
    need(setup[:5] == (FATAL_MAGIC, 1, 1152, *identity) and setup[5] == 1
        and setup[6] <= 0xFFFFFFFF and not any(setup[7:15]) and setup[15] == FATAL_MAGIC
        and not any(fatal[128:1152]))
    need(record[:5] == setup[:5] and record[5:7] == (2, setup[6]) and record[7] in (0, 1)
        and record[8] == 0xC0000005 and record[9] in (0, 1) and record[15] == FATAL_MAGIC)
    length, path_status = record[13:15]
    need(((path_status == 0 and length == 0) or (path_status == 1 and 0 < length < 512)
        or (path_status == 2 and length == 512)) and not any(fatal[1152+128+length*2:]))
    if not status_raw:
        raise CaptureRejected("capture_status_empty")
    if not wait_raw:
        raise CaptureRejected("capture_wait_empty")
    need(len(status_raw) <= 4096 and len(wait_raw) == 64)
    status = strict_json(status_raw)
    need(type(status) is dict and set(status) == STATUS_FIELDS
        and status["schema"] == "pim.private-owned-minidump.v1")
    need(status["status"] in ("complete", "failed"))
    for key in ("pid", "creation", "thread_id", "exception_code", "filter_result", "original_rip",
            "original_rsp", "original_context_flags", "dump_bytes", "deadline_tick_ms", "thread_creation",
            "exception_address", "exception_flags", "exception_parameter_count"):
        integer(status[key])
    need(status["source_commit"] == source_commit and re.fullmatch("[0-9a-f]{40}", source_commit) is not None
        and status["thread_creation"] > 0 and status["exception_flags"] <= 0xFFFFFFFF
        and status["exception_parameter_count"] <= 15 and type(status["exception_parameters"]) is list
        and len(status["exception_parameters"]) == 15 and type(status["integer_registers"]) is list
        and len(status["integer_registers"]) == 17)
    for value in status["exception_parameters"]+status["integer_registers"]:
        integer(value)
    need(status["original_context_flags"] <= 0xFFFFFFFF
        and status["original_context_flags"] & 0xFF0000 == 0x100000
        and status["original_context_flags"] & 3 == 3
        and status["integer_registers"][4] == status["original_rsp"]
        and status["integer_registers"][16] == status["original_rip"])
    need((status["pid"], status["creation"]) == identity and 0 < status["thread_id"] <= 0xFFFFFFFF
        and status["exception_code"] == record[8] and status["filter_result"] == record[9]
        and status["exception_address"] == record[10] and status["original_rsp"] > 0
        and status["basic_context_only"] is True and status["thread_owner_pid_verified"] is True
        and status["one_attempt"] is True and status["dump_bytes"] <= MAX_DUMP
        and status["deadline_tick_ms"] == expected_deadline)
    if status["status"] == "failed":
        raise CaptureRejected("capture_writer_failed")
    need(status["dump_bytes"] >= 32)
    wait = unpack("<8Q", wait_raw)
    need(wait[:5] == (WAIT_MAGIC, 1, *identity, status["thread_id"])
         and wait[7] == expected_deadline)
    if wait[5] == 258:
        raise CaptureRejected("capture_wait_timeout")
    if wait[5:7] != (0, 4):
        raise CaptureRejected("capture_wait_not_complete")
    return status


def admitted_module(row, admissions):
    path = local_path(row["path"])
    matches = [item for item in admissions if local_path(item["image"]) == path]
    need(len(matches) == 1)
    item = matches[0]
    name = ntpath.basename(path)
    need(name in OWN and digest(item["image"]) == item["image_sha256"]
        and digest(item["pdb"]) == item["pdb_sha256"])
    image = pe(bounded(item["image"], MAX_BINARY))
    pdb_cv = pdb_identity(bounded(item["pdb"], MAX_BINARY))
    need(row["cv"] is not None and row["cv"] == image["cv"] == pdb_cv
        and row["size"] == image["size"] and row["stamp"] == image["stamp"]
        and pdb_path_matches(image["pdb_path"], item["image"], item["pdb"]))
    return item, image


def clean_environment(private):
    # Construct a small environment rather than inheriting symbol/source/proxy
    # configuration. No credential, symbol-server or user extension directories.
    return {key:value for key, value in {
        "SystemRoot":r"C:\Windows", "WINDIR":r"C:\Windows", "COMSPEC":r"C:\Windows\System32\cmd.exe",
        "TEMP":str(private), "TMP":str(private), "PATH":r"C:\Windows\System32",
        "_NT_SYMBOL_PATH":"", "_NT_ALT_SYMBOL_PATH":"", "_NT_SOURCE_PATH":"",
        "_NT_DEBUGGER_EXTENSION_PATH":str(private/"empty"), "DBGHELP_HOMEDIR":str(private/"empty"),
        "LLVM_DISABLE_SYMBOLIZATION":"1", "DEBUGINFOD_URLS":"", "HOME":str(private/"empty"),
        "USERPROFILE":str(private/"empty")}.items()}


def private_tool(arguments, private, name, end, environment=None):
    """One process, private regular-file output, no retries/fallbacks."""
    need(time.monotonic() < end)
    output, errors = private/(name+".stdout.private"), private/(name+".stderr.private")
    with output.open("xb") as out, errors.open("xb") as err:
        child = subprocess.Popen(arguments, stdin=subprocess.DEVNULL, stdout=out, stderr=err,
            cwd=private, env=environment or clean_environment(private),
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        # No forceful termination: expiry is inconclusive, the outer original
        # fixture fence remains authoritative. Never wait again or launch a retry.
        child.wait(timeout=max(.001, end-time.monotonic()))
    need(child.returncode == 0 and time.monotonic() < end)
    raw, stderr = bounded(output, MAX_TOOL), bounded(errors, MAX_TOOL)
    return raw, stderr


def verify_tools(private, end):
    need(os.name == "nt" and os.environ.get("GITHUB_ACTIONS") == "true"
        and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted")
    rows = []
    for name, (root, sha, version, signature) in TOOLS.items():
        path = ntpath.join(root, name)
        need(digest(path) == sha)
        rows.append((path, version, signature))
    # Fixed text, fixed paths, no shell interpolation from any external input.
    command = "$ErrorActionPreference='Stop';@(" + ",".join("'"+row[0]+"'" for row in rows) + ")|ForEach-Object { $f=Get-Item -LiteralPath $_; [ordered]@{version=$f.VersionInfo.FileVersionRaw.ToString();signature=(Get-AuthenticodeSignature -LiteralPath $_).Status.ToString()} }|ConvertTo-Json -Compress"
    raw, _ = private_tool([POWERSHELL, "-NoLogo", "-NoProfile", "-NonInteractive", "-Command", command],
        private, "tool-metadata", end)
    actual = strict_json(raw)
    need(type(actual) is list and len(actual) == len(rows))
    for observed, (_, version, signature) in zip(actual, rows):
        need(observed == {"version":version, "signature":signature})
    for name, symbols in {"dbghelp.dll":{b"MiniDumpWriteDump"},
            "dbgcore.dll":{b"MiniDumpWriteDump"}, "dbgeng.dll":{b"DebugCreate"}}.items():
        need(symbols <= pe(bounded(ntpath.join(KITS, name), MAX_BINARY), require_codeview=False)["exports"])


def empty(reason="unavailable"):
    need(reason in {"unavailable", "deadline_exhausted", "ineligible", "unmatched_or_partial",
        "capture_records_unavailable", "capture_identity_unconfirmed",
        "capture_status_empty", "capture_wait_empty", "capture_writer_failed",
        "capture_wait_timeout", "capture_wait_not_complete",
        "dump_bytes_unconfirmed", "dump_structure_unconfirmed",
        "analysis_unavailable", "no_credible_own_frame", "observed"})
    return {"status":"inconclusive", "reason":reason, "frames":[],
        "capture_perturbed_timing":True, "loaded_module_hash_proved":False,
        "source_mapping_proves_loaded_bytes":False}


def parse_cdb(raw, snapshot):
    """Only marked numeric frames plus call instructions enter private evidence.

    CDB may heuristically walk a corrupt/nonimage PC. Such a frame remains a
    callsite candidate even if its preceding instruction really is a call.
    """
    need(len(raw) <= MAX_TOOL)
    text = raw.decode("ascii", errors="replace")
    lines = text.splitlines()
    def marked(begin, finish):
        starts = [index for index, line in enumerate(lines) if line.strip() == begin]
        ends = [index for index, line in enumerate(lines) if line.strip() == finish]
        need(len(starts) == len(ends) == 1 and starts[0] < ends[0])
        return "\n".join(lines[starts[0]+1:ends[0]])
    stack = marked("PIM_STACK_BEGIN", "PIM_STACK_END")
    rows = []
    for line in stack.splitlines():
        match = re.match(r"^\s*([0-9a-fA-F]{2})\s+([0-9a-fA-F`]{8,17})\s+([0-9a-fA-F`]{8,17})\s+", line)
        if match:
            index, sp, ret = (int(value.replace("`", ""), 16) for value in match.groups())
            need(index == len(rows) and index < 32)
            rows.append((sp, ret))
    need(rows and rows[0][0] == snapshot["rsp"])
    uncertain = bool(re.search(r"(?i)unwind information not available|frames may be wrong|following frames may be wrong|unable to read|memory read error", stack))
    result = []
    for index in range(1, min(len(rows), 32)):
        begin, finish = f"PIM_FRAME_{index:02d}_BEGIN", f"PIM_FRAME_{index:02d}_END"
        try:
            block = marked(begin, finish)
        except Rejected:
            continue
        marker = re.search(r"PIM_REG ([0-9a-fA-F`]{8,17}) ([0-9a-fA-F`]{8,17})", block)
        if marker is None:
            continue
        ip, sp = (int(value.replace("`", ""), 16) for value in marker.groups())
        if ip != rows[index-1][1] or sp != rows[index][0]:
            continue
        call = False
        for line in block.splitlines():
            match = re.match(r"^\s*([0-9a-fA-F`]{8,17})\s+([0-9a-fA-F]{2,30})\s+call\s+", line)
            if match and len(match[2]) % 2 == 0:
                call |= int(match[1].replace("`", ""), 16)+len(match[2])//2 == ip
        if call:
            result.append((ip, "uncertain_callsite_candidate" if uncertain else "callsite_candidate"))
    return result[:31]


def source_location(raw, source, allowed):
    """LLVM JSON is private. Export exact allowlisted repo-relative source only."""
    rows = strict_json(raw)
    need(type(rows) is list and len(rows) == 1 and type(rows[0]) is dict)
    symbols = rows[0].get("Symbol")
    need(type(symbols) is list and 1 <= len(symbols) <= 32)
    row = symbols[0]
    filename, function, line = row.get("FileName"), row.get("FunctionName"), row.get("Line")
    path = local_path(filename)
    root = local_path(str(source)).rstrip("\\")+"\\"
    need(path.startswith(root))
    relative = path[len(root):].replace("\\", "/")
    actual = [name for name in allowed if name.lower() == relative]
    need(len(actual) == 1)
    relative = actual[0]
    need(re.fullmatch(r"[A-Za-z0-9_./-]+", relative) is not None)
    need(type(function) is str and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_:<> ,&*\[\]().~-]{0,191}", function) is not None)
    need(type(line) is int and 1 <= line <= 1000000)
    # Hash the actual same-source file; no source-server fetch or inferred path.
    need(digest(Path(source)/relative) == allowed[relative])
    return {"file":relative, "function":function, "line":line}


def analyze(snapshot, admissions, dump_path, private, source, source_files, end):
    verified = []
    for row in snapshot["modules"]:
        try:
            item, image = admitted_module(row, admissions)
            verified.append((row, item, image))
        except (ValueError, OSError, UnicodeError, struct.error):
            continue
    need(verified)
    # Explicit local symbol/image search. IGNORE_CVREC prevents embedded PDB paths;
    # SECURE+DISABLE_SYMSRV_AUTODETECT prohibit automatic symbol-server use.
    # No source path, extension command, shell command, or source-server loading.
    symbol_dirs = sorted({str(Path(item["pdb"]).parent) for _, item, _ in verified})
    image_dirs = sorted({str(Path(item["image"]).parent) for _, item, _ in verified})
    for path in symbol_dirs+image_dirs:
        local_path(path)
    commands = [".srcpath", ".ecxr", ".echo PIM_STACK_BEGIN", "kn 20", ".echo PIM_STACK_END"]
    for index in range(1, 32):
        commands.extend([f".echo PIM_FRAME_{index:02d}_BEGIN", f".frame /r {index:x}",
            '.printf "PIM_REG %p %p\\n", @rip, @rsp', "ub @rip L8", f".echo PIM_FRAME_{index:02d}_END"])
    commands.append("q")
    command_file = private/"offline.commands.private.txt"
    command_file.write_text(";".join(commands)+"\n", encoding="ascii")
    raw, _ = private_tool([ntpath.join(KITS, "cdb.exe"), "-sins", "-snul", "-secure",
        "-sflags", "0x020e1784", "-y", ";".join(symbol_dirs), "-i", ";".join(image_dirs),
        "-z", str(dump_path), "-cf", str(command_file)], private, "cdb", end)
    evidence = parse_cdb(raw, snapshot)
    frames = []
    # The original exception PC itself is valid source evidence only in an
    # admitted image. Callsite evidence may still identify a caller for bad PCs.
    for ip, confidence in [(snapshot["rip"], "exception_instruction")]+evidence:
        need(time.monotonic() < end)
        match = [(row, item, image) for row, item, image in verified if row["base"] <= ip < row["base"]+row["size"]]
        if len(match) != 1:
            continue
        row, item, image = match[0]
        rva = ip-row["base"]
        frame = {"evidence":confidence, "own_module_basename":ntpath.basename(row["path"]).lower(),
            "admitted_module_sha256":item["image_sha256"], "instruction_rva":rva, "source":None}
        # LLVM reads only the exactly matched local PE/PDB. This does not prove
        # that loaded (possibly patched) instructions equal the on-disk image.
        try:
            output, _ = private_tool([ntpath.join(LLVM, "llvm-symbolizer.exe"), "--no-inlines",
                "--no-demangle", "--relative-address", "--output-style=JSON", "--obj="+item["image"],
                hex(max(0, rva-(confidence != "exception_instruction")))],
                private, f"symbol-{len(frames):02d}", end)
            frame["source"] = source_location(output, source, source_files)
        except (ValueError, OSError, UnicodeError, subprocess.SubprocessError):
            pass
        need(time.monotonic() < end)
        frames.append(frame)
        if len(frames) == 32:
            break
    public = empty("observed" if frames else "no_credible_own_frame")
    public.update(status="observed" if frames else "inconclusive", frames=frames)
    return public


def collect(private, metadata, identities, exit_classes, end):
    result = {"schema":"pim.private-dump-decision.v1", "diagnostic_only":True,
        "functional_acceptance":False, "capture_perturbed_timing":True,
        "target_a":empty(), "target_b":empty()}
    for index, label in enumerate(("target_a", "target_b")):
        if time.monotonic() >= end:
            result[label] = empty("deadline_exhausted")
            continue
        identity = identities.get(label)
        if identity is None or exit_classes.get(label) != "access_violation":
            result[label] = empty("ineligible")
            continue
        try:
            reason = "capture_records_unavailable"
            fatal = bounded(private/f"target-{index}.fatal.private.bin", 2304)
            status_raw = bounded(private/f"target-{index}.dump-status.private.json", 4096)
            wait = bounded(private/f"target-{index}.dump-wait.private.bin", 64)
            reason = "capture_identity_unconfirmed"
            status = owned_records(fatal, status_raw, wait, identity,
                exit_classes[label], metadata["deadline_tick_ms"], metadata["source_commit"])
            reason = "dump_bytes_unconfirmed"
            dump_path = private/f"target-{index}.dump.private.dmp"
            raw = bounded(dump_path, MAX_DUMP)
            need(len(raw) == status["dump_bytes"])
            reason = "dump_structure_unconfirmed"
            snapshot = dump(raw, status)
        except CaptureRejected as failure:
            result[label] = empty(failure.reason)
            continue
        except (ValueError, OSError, UnicodeError, struct.error):
            result[label] = empty(reason)
            continue
        try:
            target_root = private/(label+"-offline-private")
            target_root.mkdir()
            (target_root/"empty").mkdir()
            result[label] = analyze(snapshot, metadata["modules"], dump_path, target_root,
                metadata["source"], metadata["source_files"], end)
        except (ValueError, OSError, UnicodeError, struct.error, subprocess.SubprocessError):
            result[label] = empty("analysis_unavailable")
        if time.monotonic() >= end:
            result[label] = empty("deadline_exhausted")
    return result


def prepare(gate):
    """Best effort, before the existing target launch, without a ready wait."""
    root = getattr(gate.args, "private_dump_root", None)
    if root is None:
        return None
    gate.private_dump_requested = True
    gate.private_dump_prepare_phase = "root"
    need(not root.exists())
    root.mkdir()
    (root/"empty").mkdir()
    gate.private_dump_prepare_phase = "metadata"
    metadata = strict_json(bounded(gate.args.private_dump_metadata, 1024*1024))
    need(metadata["schema"] == "pim.private-dump-build.v1" and metadata["source_commit"] == gate.args.commit
        and metadata["profile"] == "debug" and metadata["dev_debug"] == 2
        and metadata["features"] == ["software-renderer"]
        and metadata["target"] == "x86_64-pc-windows-msvc"
        and local_path(metadata["source"]) == local_path(str(gate.args.source.resolve()))
        and type(metadata["source_files"]) is dict and len(metadata["source_files"]) <= 4096)
    gate.private_dump_prepare_phase = "probe_file_hashes"
    probe = metadata["probe"]
    need(ntpath.basename(probe["image"]).lower() == "protoinputmultiplex.exe"
        and digest(probe["image"]) == probe["image_sha256"] and digest(probe["pdb"]) == probe["pdb_sha256"])
    gate.private_dump_prepare_phase = "probe_pe_codeview"
    probe_image = pe(bounded(probe["image"], MAX_BINARY))
    gate.private_dump_prepare_phase = "probe_pdb_identity"
    probe_pdb_identity = pdb_identity(bounded(probe["pdb"], MAX_BINARY))
    gate.private_dump_prepare_phase = "probe_identity_match"
    need(probe_image["cv"] == probe_pdb_identity
        and pdb_path_matches(probe_image["pdb_path"], probe["image"], probe["pdb"]))
    # Exact diagnostic A/B image admission is independent of dump-tool usability.
    # A later tool failure leaves capture unarmed and the diagnostic result inconclusive.
    gate.private_dump_probe = Path(probe["image"])
    gate.private_dump_prepare_phase = "toolchain_validation"
    verify_tools(root, gate.end)
    gate.private_dump_prepare_phase = "helper_admission"
    helper = metadata["helper"]
    need(ntpath.basename(helper["file"]).lower() == "provider_private_dump.exe"
        and digest(helper["file"]) == helper["sha256"])
    gate.private_dump_prepare_phase = "module_admission"
    modules = metadata["modules"]
    need(type(modules) is list and 1 <= len(modules) <= 8 and probe in modules)
    for item in modules:
        need(ntpath.basename(local_path(item["image"])) in OWN and digest(item["image"]) == item["image_sha256"])
    # Tick sampled BEFORE monotonic gives a conservative projection: elapsed
    # time between samples only shortens the original absolute deadline.
    gate.private_dump_prepare_phase = "deadline_config"
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.GetTickCount64.restype = ctypes.c_ulonglong
    tick = kernel.GetTickCount64()
    now = time.monotonic()
    need(now < gate.end)
    deadline = tick+max(0, int((gate.end-now)*1000))
    config = {"schema":"pim.private-dump-config.v1", "source_commit":gate.args.commit,
        "helper":str(Path(helper["file"]).resolve()), "deadline_tick_ms":deadline,
        "module_paths":[item["image"] for item in modules]}
    config_path = root/"helper-config.private.json"
    config_path.write_text(json.dumps(config), encoding="utf-8")
    metadata["deadline_tick_ms"] = deadline
    gate.private_dump_metadata = metadata
    gate.private_dump_config = config_path
    gate.private_dump_probe = Path(probe["image"])
    gate.private_dump_prepare_phase = "ready"
    return config_path


def finish(gate):
    """Post-cleanup only. Do not mutate any existing failure/cleanup field."""
    if not getattr(gate, "private_dump_requested", False):
        return
    decision = {"schema":"pim.private-dump-decision.v1", "diagnostic_only":True,
        "functional_acceptance":False, "capture_perturbed_timing":True,
        "target_a":empty(), "target_b":empty()}
    metadata = getattr(gate, "private_dump_metadata", None)
    if metadata is not None:
        exits = {label:gate.result.get("target_termination_diagnostics", {}).get(label, {}).get("exit_class")
            for label in ("target_a", "target_b")}
        decision = collect(gate.private, metadata, getattr(gate, "fatal_owned_identities", {}), exits, gate.end)
    phase = getattr(gate, "private_dump_prepare_phase", "not_started")
    if not isinstance(phase, str) or phase not in PREPARE_PHASES:
        phase = "not_started"
    decision["preparation"] = {
        "status":"ready" if metadata is not None and phase == "ready" else "not_ready",
        "phase":phase,
    }
    gate.result["private_dump_diagnostics"] = decision
    # A capture-enabled group can never become functional acceptance, including
    # the otherwise-successful path. Preserve all preexisting FAIL details.
    gate.result["diagnostic_only"] = True
    gate.result["functional_acceptance"] = False
    if gate.result.get("status") == "PASS":
        gate.result["status"] = "DIAGNOSTIC_ONLY"
    output = getattr(gate.args, "private_dump_decision", None)
    if output is not None:
        need(not output.exists())
        output.write_text(json.dumps(decision, indent=2)+"\n", encoding="utf-8")
