"""Stage audited build outputs; stdlib only. Build with package-portable.ps1 first."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parent.parent
PIN = "dd29bf8d22349605d0e1950c1c5dc4893d72670e"
RUNTIME = [f"ProtoInput{kind}{bits}.{ext}" for bits in (32, 64)
           for kind, ext in (("Hooks", "dll"), ("IJ", "exe"), ("IJP", "dll"), ("Loader", "dll"))]
# Separate offline model/data bundle; never part of the ProtoInput DLL closure.
OCR_FILES = (
    "text-detection-ssfbcj81.rten", "text-rec-checkpoint-s52qdbqt.rten",
    "MODEL_CARD.md", "provenance.json", "LICENSE-CC-BY-SA-4.0.txt", "ATTRIBUTION.md",
)
OCR_MODEL_PINS = {
    "text-detection-ssfbcj81.rten": (2523564, "614aafabf27c94d386f7aa036c967c2e47e4b9938fa11531ca8f5698c1ca4c36"),
    "text-rec-checkpoint-s52qdbqt.rten": (9716444, "606d9a0414c6b73c99df75b707c11c70d1c8b12e1d4f900922e185fc37bfca65"),
}
SDK_TARGETS = {"x64": "x86_64-pc-windows-msvc", "x86": "i686-pc-windows-msvc", "web": "wasm32-unknown-unknown"}
SDK_FEATURES = ["native-client", "native-ffi", "native-windows"]
BINDGEN_VERSION = "0.2.129"
BINDGEN_SHA = "79e348c169d0c10c0b287647f1e753d0985353ca96800bc29be8f4261baf13e6"
SDK_NATIVE_FILES = ["mux_application_sdk.dll", "protoinputmultiplex_application_sdk.h"]
SDK_WEB_FILES = ["index.html", "bootstrap.js", "pkg/mux_application_web.js",
                 "pkg/mux_application_web.d.ts", "pkg/mux_application_web_bg.wasm",
                 "pkg/mux_application_web_bg.wasm.d.ts"]
REQUIRED_SOURCE_FILES = (
    "Cargo.toml", "Cargo.lock", "scripts/package-portable.ps1", "scripts/package-portable.py",
    "scripts/test-package-sdk.py", "scripts/build-backend.ps1", ".github/workflows/rust.yml",
    "crates/mux-application-sdk/Cargo.toml", "crates/mux-application-sdk/build.rs",
    "crates/mux-application-sdk/src/abi_contract.rs", "crates/mux-application-web/Cargo.toml",
    "crates/mux-application-web/example/index.html", "crates/mux-application-web/example/bootstrap.js",
)
BACKEND_PATCH_SEQUENCE = (
    "patches/0001-backend.patch",
    "patches/lifecycle/0004-clean-restart.patch",
    "patches/keyboard-state/0005-consumed-keyboard-state.patch",
    "patches/lifecycle/0006-checked-message-filter-setup.patch",
    "patches/stdio/0007-preserve-target-stdio.patch",
)
# OS components only. Unknown imports fail staging, including VC redistributables.
SYSTEM = set("advapi32 avrt bcrypt bcryptprimitives cfgmgr32 combase comctl32 comdlg32 crypt32 cryptbase d2d1 d3d11 d3d12 d3dcompiler_47 dbghelp dbgcore dcomp dhcpcsvc dinput8 dnsapi dwrite dwmapi dxgi gdi32 hid imm32 iphlpapi kernel32 kernelbase mpr msvcrt mswsock ncrypt netapi32 normaliz ntdll ole32 oleaut32 opengl32 powrprof propsys psapi rpcrt4 secur32 setupapi shell32 shcore shlwapi ucrtbase uiautomationcore user32 userenv usp10 uxtheme version winhttp wininet winmm winspool wintrust wldap32 ws2_32 wtsapi32".split())


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def app_features(renderer):
    if renderer not in ("glow", "software-dx12"):
        raise ValueError("Unknown GUI renderer build choice")
    return ["software-renderer"] if renderer == "software-dx12" else []


def metadata_features(renderer):
    return ",".join(["mux-application-sdk/native-client"] +
                    ["protoinputmultiplex/" + name for name in app_features(renderer)])


def app_build_record(target, build_inputs, renderer):
    if build_inputs is None or build_inputs.get("gui_renderer") != renderer:
        raise ValueError("GUI renderer differs from frozen build inputs")
    target = target.resolve()
    log = target/"app-build.jsonl"
    records = [json.loads(line) for line in log.read_text(encoding="utf-8-sig").splitlines() if line.strip()]
    artifacts = [row for row in records if row.get("reason") == "compiler-artifact"
                 and row.get("target", {}).get("name") == "protoinputmultiplex"
                 and row["target"].get("kind") == ["bin"]]
    if len(artifacts) != 1:
        raise ValueError("Expected one exact application compiler artifact")
    artifact = artifacts[0]
    executable = target/"x86_64-pc-windows-msvc/release/protoinputmultiplex.exe"
    if (sorted(artifact["features"]) != app_features(renderer)
            or not artifact.get("executable") or Path(artifact["executable"]).resolve() != executable
            or artifact["profile"].get("debug_assertions") is not False):
        raise ValueError("Application artifact differs from renderer/target/release build")
    return {"gui_renderer": renderer, "features": artifact["features"],
            "default_renderer": renderer, "target": "x86_64-pc-windows-msvc", "profile": "release",
            "cargo_json_sha256": sha(log), "executable_sha256": sha(executable)}


def sdk_identity(build_inputs, architecture):
    if build_inputs is None:
        raise ValueError("SDK requires frozen --build-inputs")
    return {"schema_version": 1, "source_commit": build_inputs["source_commit"],
            "build_inputs_sha256": hashlib.sha256(json.dumps(build_inputs, sort_keys=True).encode()).hexdigest(),
            "target": SDK_TARGETS[architecture], "profile": "release",
            "default_features": True,
            "features": SDK_FEATURES if architecture != "web" else [],
            "rustflags": "-C target-feature=+crt-static" if architecture != "web" else "",
            "wasm_bindgen": {"version": BINDGEN_VERSION, "official_archive_sha256": BINDGEN_SHA} if architecture == "web" else None,
            "qualification": "release packaging only; native/browser runtime acceptance separate"}


def sdk_records(root, names):
    actual = sorted(p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file())
    if actual != sorted(names):
        raise ValueError(f"Missing or extra SDK files in {root}: {actual}")
    return [{"path": name, "bytes": (root/name).stat().st_size, "sha256": sha(root/name)} for name in names]


def validate_sdk_content(root, architecture):
    if architecture == "web":
        if (root/"pkg/mux_application_web_bg.wasm").read_bytes()[:8] != b"\0asm\x01\0\0\0":
            raise ValueError("Invalid browser WASM")
        for name in ("index.html", "bootstrap.js"):
            if (root/name).read_bytes() != (ROOT/"crates/mux-application-web/example"/name).read_bytes():
                raise ValueError("Browser example differs from frozen sources")
    else:
        if pe_imports(root/SDK_NATIVE_FILES[0])["machine"] != architecture:
            raise ValueError("SDK DLL architecture mismatch")
        header = (root/SDK_NATIVE_FILES[1]).read_text(encoding="utf-8")
        required = (
            "PIM_APP_NATIVE_CLIENT_ABI_VERSION 3u",
            "pim_app_native_completion_handle",
            "pim_app_native_window_register",
            "pim_app_native_window_refresh",
            "pim_app_native_window_notify",
            "pim_app_native_window_retire",
            "pim_app_native_local_toolkit",
        )
        if any(symbol not in header for symbol in required):
            raise ValueError("SDK header lacks native-client ABI v3 consumer exports")


def record_sdk(args, build_inputs):
    target = args.target_dir.resolve()
    architecture = "web" if args.record_sdk_web else next(k for k, v in SDK_TARGETS.items() if v == args.sdk_target)
    root = target/"sdk-release"/architecture
    names = SDK_WEB_FILES if architecture == "web" else SDK_NATIVE_FILES
    identity = sdk_identity(build_inputs, architecture)
    if architecture != "web":
        build = target/"sdk-release-build"/architecture
        records = [json.loads(line) for line in args.record_sdk_native.read_text(encoding="utf-8-sig").splitlines() if line.strip()]
        artifacts = [r for r in records if r.get("reason") == "compiler-artifact" and r.get("target", {}).get("name") == "mux_application_sdk" and "cdylib" in r.get("target", {}).get("crate_types", [])]
        if len(artifacts) != 1 or sorted(artifacts[0]["features"]) != ["default"] + SDK_FEATURES:
            raise ValueError("Expected one full-default/native-client SDK artifact")
        artifact = artifacts[0]
        scripts = [r for r in records if r.get("reason") == "build-script-executed" and r.get("package_id") == artifact["package_id"]]
        if len(scripts) != 1:
            raise ValueError("Expected one matching SDK header build-script output")
        dlls = [Path(p).resolve() for p in artifact["filenames"] if Path(p).name == SDK_NATIVE_FILES[0]]
        expected_dll = (build/args.sdk_target/"release"/SDK_NATIVE_FILES[0]).resolve()
        header = (Path(scripts[0]["out_dir"])/SDK_NATIVE_FILES[1]).resolve()
        if dlls != [expected_dll] or not header.is_relative_to((build/args.sdk_target/"release/build").resolve()):
            raise ValueError("SDK artifacts outside exact target/profile build")
        root.mkdir(parents=True, exist_ok=False)
        for source, name in zip((expected_dll, header), names):
            shutil.copyfile(source, root/name)
            if sha(source) != sha(root/name):
                raise ValueError("SDK changed while recording")
    validate_sdk_content(root, architecture)
    identity["files"] = sdk_records(root, names)
    (root/"metadata.json").write_text(json.dumps(identity, indent=2)+"\n", encoding="utf-8")


def sdk_inputs(target, build_inputs, directory="sdk-release"):
    inputs, metadata = [], {}
    for architecture in SDK_TARGETS:
        root = target/directory/architecture
        record = json.loads((root/"metadata.json").read_text(encoding="utf-8"))
        names = SDK_WEB_FILES if architecture == "web" else SDK_NATIVE_FILES
        expected = sdk_identity(build_inputs, architecture)
        identity = {k: v for k, v in record.items() if k != "files"}
        if identity != expected:
            raise ValueError(f"SDK metadata identity mismatch: {architecture}")
        # Check the exact fixed closure, including a separately hashed metadata file.
        actual = sorted(p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file())
        if actual != sorted(names + ["metadata.json"]):
            raise ValueError("Missing/extra SDK release payload")
        observed = [{"path": n, "bytes": (root/n).stat().st_size, "sha256": sha(root/n)} for n in names]
        if record["files"] != observed:
            raise ValueError("SDK digest mismatch")
        validate_sdk_content(root, architecture)
        inputs += [(root/n, Path("sdk")/architecture/n) for n in names + ["metadata.json"]]
        metadata[architecture] = record
    return inputs, metadata


def validate_ocr_assets(built_assets, source_assets):
    """Validate only fixed filenames against frozen source assets and model pins."""
    records = []
    for name in OCR_FILES:
        source, built = source_assets/name, built_assets/name
        for path in (source, built):
            if not path.is_file():
                raise FileNotFoundError(path)
        expected = (source.stat().st_size, sha(source))
        if name in OCR_MODEL_PINS and expected != OCR_MODEL_PINS[name]:
            raise ValueError(f"Unapproved OCR source model: {name}")
        if (built.stat().st_size, sha(built)) != expected:
            raise ValueError(f"OCR asset differs from frozen source: {name}")
        records.append({"name": name, "bytes": expected[0], "sha256": expected[1]})
    return records


def copy_ocr_assets(built_assets, source_assets, destination):
    """Stage the complete validated model and notice closure without discovery."""
    records = validate_ocr_assets(built_assets, source_assets)
    destination.mkdir(parents=True, exist_ok=False)
    for item in records:
        target = destination/item["name"]
        shutil.copyfile(built_assets/item["name"], target)
        if (target.stat().st_size, sha(target)) != (item["bytes"], item["sha256"]):
            raise ValueError(f"OCR asset changed during staging: {item['name']}")
    return records


def validate_ocrs_license(source_notice):
    """Require the pinned upstream notice bytes before compilation or staging."""
    expected = (1070, "96b5371602fbd3cb11a680dade714465520c72b02397be5095a3c29b1a3fb30a")
    data = source_notice.read_bytes()
    if (len(data), hashlib.sha256(data).hexdigest()) != expected:
        raise ValueError("Unapproved ocrs 0.12.2 MIT notice")
    return expected


def copy_ocrs_license(source_notice, destination):
    """Supply the exact upstream MIT terms omitted from ocrs 0.12.2's crate."""
    expected = validate_ocrs_license(source_notice)
    destination.mkdir(parents=True, exist_ok=True)
    target = destination/"LICENSE-MIT.txt"
    shutil.copyfile(source_notice, target)
    if (target.stat().st_size, sha(target)) != expected:
        raise ValueError("ocrs MIT notice changed during staging")
    return ["LICENSE-MIT.txt"]


def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True, encoding="utf-8").strip()


def source_commit():
    result = subprocess.run(["git", "rev-parse", "--verify", "HEAD"], cwd=ROOT,
                            text=True, capture_output=True)
    return result.stdout.strip() if result.returncode == 0 else None


def native_tools():
    vswhere = Path(os.environ["ProgramFiles(x86)"])/"Microsoft Visual Studio/Installer/vswhere.exe"
    installation = Path(command(str(vswhere), "-latest", "-products", "*", "-requires", "Microsoft.Component.MSBuild", "-property", "installationPath"))
    msbuild = command(str(vswhere), "-latest", "-products", "*", "-requires", "Microsoft.Component.MSBuild", "-find", "MSBuild\\**\\Bin\\MSBuild.exe").splitlines()[0]
    tool_version = installation/"VC/Auxiliary/Build/Microsoft.VCToolsVersion.default.txt"
    return {"msbuild": command(msbuild, "-nologo", "-version"),
            "default_msvc_tools": tool_version.read_text().strip(),
            "platform_toolset": "v143", "windows_sdk_selector": "10.0"}


def pe_imports(path):
    """Read both PE import tables, without executing DLLs or using dev PATH tools."""
    data = path.read_bytes()
    def u16(off): return struct.unpack_from("<H", data, off)[0]
    def u32(off): return struct.unpack_from("<I", data, off)[0]
    pe = u32(0x3c)
    if data[:2] != b"MZ" or data[pe:pe+4] != b"PE\0\0":
        raise ValueError(f"Not a PE binary: {path}")
    machine = u16(pe+4)
    optional = pe+24
    magic = u16(optional)
    if magic not in (0x10b, 0x20b): raise ValueError("Unknown PE format")
    directories = optional + (112 if magic == 0x20b else 96)
    image_base = struct.unpack_from("<Q" if magic == 0x20b else "<I", data,
                                    optional + (24 if magic == 0x20b else 28))[0]
    section_start = optional + u16(pe+20)
    sections = [struct.unpack_from("<IIII", data, section_start+i*40+8)
                for i in range(u16(pe+6))]
    def offset(rva):
        for virtual_size, virtual, raw_size, raw in sections:
            if virtual <= rva < virtual + max(virtual_size, raw_size):
                return raw + rva - virtual
        if rva < u32(optional+60): return rva
        raise ValueError(f"Unmapped PE RVA in {path}: {rva:x}")
    def string(rva):
        start = offset(rva)
        return data[start:data.index(b"\0", start)].decode("ascii")
    imports, delayed = [], []
    rva = u32(directories+8)
    if rva:
        entry = offset(rva)
        while any(data[entry:entry+20]):
            imports.append(string(u32(entry+12)))
            entry += 20
    rva = u32(directories+13*8)
    if rva:
        entry = offset(rva)
        while any(data[entry:entry+32]):
            name = u32(entry+4)
            delayed.append(string(name if u32(entry) & 1 else name-image_base))
            entry += 32
    return {"machine": {0x8664: "x64", 0x14c: "x86"}.get(machine, hex(machine)),
            "imports": sorted(set(imports)), "delay_imports": sorted(set(delayed))}


def notice(path):
    name = path.name.lower()
    return any(token in name for token in ("license", "licence", "copying", "copyright", "notice", "credits", "ofl", "lgpl", "gpl-"))


def copy_notices(source, destination):
    copied = []
    for base, dirs, files in os.walk(source):
        dirs[:] = sorted(d for d in dirs if d not in (".git", "target", "build", "obj"))
        for name in sorted(files):
            item = Path(base)/name
            if notice(item):
                relative = item.relative_to(source)
                target = destination/relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(item, target)
                copied.append(relative.as_posix())
    return copied


def source_allowed(p):
    return bool(p and (p.startswith(("vendor/ProtoInput/", "patches/", "scripts/", "packaging/", "crates/"))
                        or (p.startswith("docs/") and p.endswith(".md"))
                        or (p.startswith("examples/rust-mcp-client/src/") and p.endswith(".rs"))
                        or (p.startswith("examples/rust-mcp-client/tests/") and p.endswith(".rs"))
                        or p in ("AGENTS.md", ".github/workflows/rust.yml", ".github/workflows/rust-mcp-client.yml", "examples/two-seats.json",
                                 "examples/rust-mcp-client/Cargo.toml", "examples/rust-mcp-client/Cargo.lock", "examples/rust-mcp-client/README.md", "examples/rust-mcp-client/LICENSE",
                                 "skills/protoinputmultiplex/SKILL.md", "skills/protoinputmultiplex/agents/openai.yaml", "skills/protoinputmultiplex/references/operations.md",
                                 "Cargo.toml", "Cargo.lock", "LICENSE", "THIRD_PARTY_NOTICES.md", "source-baseline.json", ".gitignore", ".gitattributes", ".gitmodules", "README.md", "docs/portable-packaging.md", "docs/installation-recovery.md"))
                   and "__pycache__" not in p and not p.endswith(".pyc"))


def patch_created_sources(committed):
    """Replay the complete committed builder patch order in a private Git index."""
    patches = []
    if {p.relative_to(ROOT).as_posix() for p in (ROOT/"patches").glob("*.patch")} != {BACKEND_PATCH_SEQUENCE[0]}:
        raise ValueError("Backend base patch set changed; audit builder patch order")
    for patch_name in BACKEND_PATCH_SEQUENCE:
        if patch_name not in committed:
            raise ValueError("Required compatibility patch is not committed")
        patch_bytes = subprocess.check_output(["git", "show", "HEAD:"+patch_name], cwd=ROOT)
        if (ROOT/patch_name).read_bytes().replace(b"\r\n", b"\n") != patch_bytes.replace(b"\r\n", b"\n"):
            raise ValueError("Compatibility admission patch differs from committed HEAD")
        patches.append(patch_bytes)
    vendor = (ROOT/"vendor/ProtoInput").resolve()
    if subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=vendor).decode().strip() != PIN:
        raise ValueError("Native source HEAD differs from pinned backend")
    objects = Path(subprocess.check_output(["git", "rev-parse", "--git-path", "objects"], cwd=vendor).decode().strip())
    if not objects.is_absolute():
        objects = vendor/objects
    with tempfile.TemporaryDirectory(prefix="protoinputmux-source-index-") as temporary:
        scratch = Path(temporary)
        object_dir = scratch/"objects"
        object_dir.mkdir()
        environment = dict(os.environ, GIT_INDEX_FILE=str(scratch/"index"),
                           GIT_OBJECT_DIRECTORY=str(object_dir),
                           GIT_ALTERNATE_OBJECT_DIRECTORIES=json.dumps(str(objects.resolve()), ensure_ascii=False),
                           GIT_OPTIONAL_LOCKS="0")
        def index_command(*args):
            return subprocess.check_output(["git", "-c", "core.autocrlf=false", "-c", "core.splitIndex=false",
                                            "-c", "core.fsmonitor=false", *args], cwd=vendor, env=environment)
        index_command("read-tree", PIN)
        for number, data in enumerate(patches):
            patch_file = scratch/f"{number}.patch"
            patch_file.write_bytes(data)
            flags = ["--cached", "--ignore-space-change"]
            if number:
                flags.append("--whitespace=nowarn")
            index_command("apply", *flags, str(patch_file))
        added = index_command("diff", "--cached", "--no-renames", "--diff-filter=A", "--name-only", "-z", PIN).decode().split("\0")
        admitted = set()
        for relative in filter(None, added):
            path = vendor/relative
            if (not relative.startswith("src/ProtoInput/") or Path(relative).suffix not in (".h", ".hpp", ".cpp", ".c", ".inl")
                    or not path.resolve().is_relative_to(vendor) or not path.is_file()):
                raise ValueError("Prepared patch-created source missing or outside approved source tree")
            expected = index_command("show", ":"+relative)
            if path.read_bytes().replace(b"\r\n", b"\n") != expected.replace(b"\r\n", b"\n"):
                raise ValueError("Prepared compatibility header differs from final ordered patch closure")
            admitted.add("vendor/ProtoInput/"+relative)
        return admitted


def committed_source_inventory(repository, prefix="", approved=None):
    # HEAD, rather than the index, is the release admission boundary. Tracked
    # compatibility patches may modify current bytes; frozen hashes cover them.
    committed = {prefix+p for p in subprocess.check_output(
        ["git", "ls-tree", "-r", "--name-only", "-z", "HEAD"], cwd=repository).decode().split("\0") if p}
    candidates = {prefix+p for p in subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=repository).decode().split("\0") if p}
    approved = approved or set()
    if any(source_allowed(p) for p in candidates - committed - approved):
        # Do not print private filenames or contents into distribution logs.
        raise ValueError("Uncommitted source candidates present; commit release sources and keep private files outside source roots")
    return committed | approved


def source_files():
    names = committed_source_inventory(ROOT)
    vendor = ROOT/"vendor/ProtoInput"
    if (vendor/".git").exists():
        # Git's full patch engine handles every new-file diff style and later
        # edits; only final added source blobs can bypass untracked rejection.
        approved = patch_created_sources(names)
        names |= committed_source_inventory(vendor, "vendor/ProtoInput/", approved)
    # source-baseline.json remains provenance data, never an admission authority.
    # A flattened checkout must commit even ignored pinned vendor files first.
    selected = {p for p in names if source_allowed(p)}
    if not set(REQUIRED_SOURCE_FILES).issubset(selected):
        raise ValueError("Required release sources are not committed in HEAD")
    if any(not (ROOT/p).is_file() for p in selected):
        raise ValueError("Committed release source missing from working tree")
    return sorted(selected)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--record-inputs", type=Path, help="Record frozen inputs before building, then exit")
    parser.add_argument("--build-inputs", type=Path, help="Verify frozen build inputs during staging")
    parser.add_argument("--record-sdk-native", type=Path, help="Cargo JSON from a fresh isolated native SDK build")
    parser.add_argument("--sdk-target", choices=list(SDK_TARGETS.values())[:2])
    parser.add_argument("--record-sdk-web", action="store_true")
    parser.add_argument("--gui-renderer", choices=("glow", "software-dx12"), default="glow")
    args = parser.parse_args()
    if args.record_inputs:
        validate_ocrs_license(ROOT/"crates/protoinputmultiplex/notices/ocrs-MIT.txt")
        files = [{"path": p, "sha256": sha(ROOT/p)} for p in source_files()]
        record = {"schema_version": 1, "source_commit": source_commit(), "source_files": files,
                  "gui_renderer": args.gui_renderer,
                  "rustc": command("rustc", "-Vv"), "cargo": command("cargo", "-V"),
                  "native_tools": native_tools(), "expected_rustflags": "-C target-feature=+crt-static"}
        args.record_inputs.parent.mkdir(parents=True, exist_ok=True)
        args.record_inputs.write_text(json.dumps(record, indent=2)+"\n", encoding="utf-8")
        print(f"Recorded {len(files)} frozen build inputs")
        return
    recording_sdk = args.record_sdk_native or args.record_sdk_web
    if not args.target_dir or (not args.output and not recording_sdk): parser.error("--output and --target-dir are required for staging")
    build_inputs = None
    if args.build_inputs:
        build_inputs = json.loads(args.build_inputs.read_text(encoding="utf-8"))
        if build_inputs.get("gui_renderer") != args.gui_renderer:
            raise ValueError("GUI renderer differs from frozen build inputs")
        recorded = {p["path"]: p["sha256"] for p in build_inputs["source_files"]}
        actual = {p: sha(ROOT/p) for p in source_files()}
        if recorded != actual or build_inputs["source_commit"] != source_commit():
            raise ValueError("Build inputs changed; freeze/apply native patches before building and retry")
    if recording_sdk:
        if bool(args.record_sdk_native) == bool(args.record_sdk_web) or (args.record_sdk_native and not args.sdk_target):
            parser.error("Select exactly one SDK recording mode; native requires --sdk-target")
        record_sdk(args, build_inputs)
        return
    output = args.output.resolve()
    if output.exists(): raise ValueError("Output must be a new directory")
    application_build = app_build_record(args.target_dir, build_inputs, args.gui_renderer)
    binary = args.target_dir.resolve()/"x86_64-pc-windows-msvc/release/protoinputmultiplex.exe"
    ocr_built = binary.parent/"assets/ocr"
    ocr_source = ROOT/"crates/protoinputmultiplex/assets/ocr"
    validate_ocr_assets(ocr_built, ocr_source)
    native = ROOT/"vendor/ProtoInput/src/ProtoInput/Release"
    inputs = [(binary, Path("protoinputmultiplex.exe"))]
    inputs += [(native/name, Path("runtime")/name) for name in RUNTIME]
    inputs += [(args.target_dir.resolve()/target/"release/cursor_observer.dll",
                Path("runtime")/f"protoinputmultiplex_cursor_observer{bits}.dll")
               for target, bits in (("x86_64-pc-windows-msvc", 64), ("i686-pc-windows-msvc", 32))]
    sdk_payload, sdk_metadata = sdk_inputs(args.target_dir.resolve(), build_inputs)
    inputs += sdk_payload
    for path, _ in inputs:
        if not path.is_file(): raise FileNotFoundError(path)
    pin_text = (ROOT/"THIRD_PARTY_NOTICES.md").read_text(encoding="utf-8")
    if PIN not in pin_text: raise ValueError("ProtoInput pin changed; audit runtime set")
    if (ROOT/"vendor/ProtoInput/.git").exists() and command("git", "-C", str(ROOT/"vendor/ProtoInput"), "rev-parse", "HEAD") != PIN:
        raise ValueError("ProtoInput checkout does not match audited pin")
    source_names = source_files()
    sources = [{"path": p, "sha256": sha(ROOT/p)} for p in source_names]
    # Include web-only dependencies and native-client optional dependencies, too.
    graphs = [json.loads(command("cargo", "--config", 'build.rustc-wrapper=""', "metadata", "--locked", "--offline",
                                 "--features", metadata_features(args.gui_renderer), "--format-version", "1", "--filter-platform", triple))
              for triple in SDK_TARGETS.values()]
    metadata = graphs[0]
    nodes = {n["id"] for graph in graphs for n in graph["resolve"]["nodes"]}
    packages = {p["id"]: p for graph in graphs for p in graph["packages"]}
    output.mkdir(parents=True)
    for path, relative in inputs:
        target = output/relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
        if sha(path) != sha(target): raise ValueError(f"Input changed while copying: {relative}")
    # Revalidate the copied SDK against its recorded identity and fixed hashes.
    sdk_inputs(output, build_inputs, "sdk")
    copy_ocr_assets(ocr_built, ocr_source, output/"assets/ocr")
    for name in ("launch-gui.cmd", "launch-mcp.cmd", "README-portable.txt"):
        shutil.copyfile(ROOT/"packaging"/name, output/name)
    shutil.copyfile(ROOT/"docs/installation-recovery.md", output/"INSTALLATION.md")
    for name in ("LICENSE", "THIRD_PARTY_NOTICES.md"):
        shutil.copyfile(ROOT/name, output/name)
    for name in ("data", "profiles", "macros"):
        (output/name).mkdir()
    copy_notices(ROOT/"vendor/ProtoInput", output/"licenses/ProtoInput")
    copy_notices(ROOT/"packaging/licenses", output/"licenses/supplemental")
    dependencies = []
    for package in sorted(packages.values(), key=lambda p: (p["name"], p["version"])):
        if package["id"] not in nodes or package["source"] is None: continue
        label = package["name"]+"-"+package["version"]
        files = copy_notices(Path(package["manifest_path"]).parent, output/"licenses/rust"/label)
        if not files and label == "ocrs-0.12.2":
            files = copy_ocrs_license(ROOT/"crates/protoinputmultiplex/notices/ocrs-MIT.txt",
                                      output/"licenses/rust"/label)
        supplement = ROOT/"packaging/licenses/rust"/label
        if not files and supplement.is_dir():
            files = copy_notices(supplement, output/"licenses/rust"/label)
        if not files:
            raise ValueError(f"Missing notice files: {label}; inspect upstream before packaging")
        dependencies.append({"name": package["name"], "version": package["version"],
                             "source": package["source"], "license": package["license"], "notices": files})
    sysroot = Path(command("rustc", "--print", "sysroot"))
    rust_licenses = sysroot/"share/doc/rust/licenses"
    if not rust_licenses.is_dir(): raise ValueError("Rust toolchain license directory missing")
    copy_notices(rust_licenses, output/"licenses/rust-toolchain")
    (output/"source").mkdir()
    with zipfile.ZipFile(output/"source/corresponding-source.zip", "w", zipfile.ZIP_DEFLATED) as archive:
        for name in source_names:
            info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, (ROOT/name).read_bytes())
    # Catch edits during collection rather than publishing misleading provenance.
    for item in sources:
        if sha(ROOT/item["path"]) != item["sha256"]: raise ValueError("Source changed during packaging")
    binaries, system = {}, set()
    available = [p for p in output.rglob("*") if p.suffix.lower() in (".exe", ".dll")]
    for path in available:
        report = pe_imports(path)
        relative = path.relative_to(output)
        expected = "x86" if path.stem.endswith("32") or relative.parts[:2] == ("sdk", "x86") else "x64"
        if report["machine"] != expected: raise ValueError(f"Unexpected architecture: {path}")
        for name in report["imports"] + report["delay_imports"]:
            lower = name.lower()
            matches = [p for p in available if p.name.lower() == lower and p.parent == path.parent]
            if matches:
                if len(matches) != 1: raise ValueError("Ambiguous colocated dependency")
                dependency = matches[0]
                if pe_imports(dependency)["machine"] != report["machine"]: raise ValueError("Dependency architecture mismatch")
            elif lower.removesuffix(".dll") in SYSTEM or lower.startswith(("api-ms-win-", "ext-ms-win-")):
                system.add(name)
            else:
                raise ValueError(f"Unresolved non-system import: {path.name}: {name}")
        binaries[path.relative_to(output).as_posix()] = report
    files = [{"path": p.relative_to(output).as_posix(), "bytes": p.stat().st_size, "sha256": sha(p)}
             for p in sorted(output.rglob("*")) if p.is_file()]
    baseline = ROOT/"source-baseline.json"
    version = next(p["version"] for p in metadata["packages"] if p["name"] == "protoinputmultiplex")
    manifest = {"schema_version": 1, "application": "ProtoInputMultiPlex", "version": version, "platform": "windows-x64",
                "protoinput_commit": PIN, "source_commit": source_commit(),
                "source_status": command("git", "status", "--short"),
                "baseline_sha256": sha(baseline) if baseline.exists() else None,
                "source_files": sources, "rustc": command("rustc", "-Vv"), "cargo": command("cargo", "-V"),
                "expected_rustflags": "-C target-feature=+crt-static (native); empty (wasm)", "expected_build_configuration": "Release / app+backend+observer x64/x86; SDK native full defaults+native-client x64/x86 and web wasm32; --locked",
                "build_inputs_verified": build_inputs is not None, "build_inputs": build_inputs,
                "application_build": application_build,
                "runtime_files": RUNTIME + ["protoinputmultiplex_cursor_observer32.dll", "protoinputmultiplex_cursor_observer64.dll"], "binaries": binaries, "host_system_imports": sorted(system),
                "dynamic_system_dependencies": ["Windows OpenGL implementation and display driver (Glow GUI)"] +
                    (["Windows DX12-compatible CPU adapter (default GUI); adapter enumeration may probe hardware drivers"]
                     if args.gui_renderer == "software-dx12" else []),
                "application_sdk": sdk_metadata, "rust_dependencies": dependencies, "files": files,
                "validation_scope": "Packaging and launch only; no routing acceptance implied"}
    (output/"bundle-manifest.json").write_text(json.dumps(manifest, indent=2)+"\n", encoding="utf-8")
    print(json.dumps({"bundle": str(output), "files": len(files), "binaries": len(binaries), "rust_notices": len(dependencies)}))


if __name__ == "__main__":
    main()
