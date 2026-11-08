"""Stage audited build outputs; stdlib only. Build with package-portable.ps1 first."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
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
# OS components only. Unknown imports fail staging, including VC redistributables.
SYSTEM = set("advapi32 avrt bcrypt bcryptprimitives cfgmgr32 combase comctl32 comdlg32 crypt32 cryptbase d2d1 d3d11 d3d12 d3dcompiler_47 dbghelp dbgcore dcomp dhcpcsvc dinput8 dnsapi dwrite dwmapi dxgi gdi32 hid imm32 iphlpapi kernel32 kernelbase mpr msvcrt mswsock ncrypt netapi32 normaliz ntdll ole32 oleaut32 opengl32 powrprof propsys psapi rpcrt4 secur32 setupapi shell32 shcore shlwapi ucrtbase uiautomationcore user32 userenv usp10 uxtheme version winhttp wininet winmm winspool wintrust wldap32 ws2_32 wtsapi32".split())


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


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


def copy_ocrs_license(source_notice, destination):
    """Supply the exact upstream MIT terms omitted from ocrs 0.12.2's crate."""
    expected = (1070, "96b5371602fbd3cb11a680dade714465520c72b02397be5095a3c29b1a3fb30a")
    data = source_notice.read_bytes()
    if (len(data), hashlib.sha256(data).hexdigest()) != expected:
        raise ValueError("Unapproved ocrs 0.12.2 MIT notice")
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


def source_files():
    # A snapshot may flatten the submodule. A normal checkout needs a second list.
    names = subprocess.check_output(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=ROOT).decode().split("\0")
    baseline = ROOT/"source-baseline.json"
    if baseline.is_file():
        # Flattened submodule files keep their original tracked identity even
        # when an upstream ignore pattern would hide them in the new Git repo.
        inventory = json.loads(baseline.read_text(encoding="utf-8"))["files"]
        names += list(inventory)
    vendor = ROOT/"vendor/ProtoInput"
    if (vendor/".git").exists():
        names += ["vendor/ProtoInput/"+p for p in subprocess.check_output(
            ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=vendor).decode().split("\0") if p]
    return sorted({p for p in names if p and (ROOT/p).is_file()
                   and (p.startswith(("vendor/ProtoInput/", "patches/", "scripts/", "packaging/", "crates/"))
                        or (p.startswith("docs/") and p.endswith(".md"))
                        or p in ("Cargo.toml", "Cargo.lock", "LICENSE", "THIRD_PARTY_NOTICES.md", "source-baseline.json", ".gitignore", ".gitattributes", ".gitmodules", "README.md", "docs/portable-packaging.md", "docs/installation-recovery.md"))
                   and "__pycache__" not in p and not p.endswith(".pyc")})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--record-inputs", type=Path, help="Record frozen inputs before building, then exit")
    parser.add_argument("--build-inputs", type=Path, help="Verify frozen build inputs during staging")
    args = parser.parse_args()
    if args.record_inputs:
        files = [{"path": p, "sha256": sha(ROOT/p)} for p in source_files()]
        record = {"schema_version": 1, "source_commit": source_commit(), "source_files": files,
                  "rustc": command("rustc", "-Vv"), "cargo": command("cargo", "-V"),
                  "native_tools": native_tools(), "expected_rustflags": "-C target-feature=+crt-static"}
        args.record_inputs.parent.mkdir(parents=True, exist_ok=True)
        args.record_inputs.write_text(json.dumps(record, indent=2)+"\n", encoding="utf-8")
        print(f"Recorded {len(files)} frozen build inputs")
        return
    if not args.output or not args.target_dir: parser.error("--output and --target-dir are required for staging")
    build_inputs = None
    if args.build_inputs:
        build_inputs = json.loads(args.build_inputs.read_text(encoding="utf-8"))
        recorded = {p["path"]: p["sha256"] for p in build_inputs["source_files"]}
        actual = {p: sha(ROOT/p) for p in source_files()}
        if recorded != actual:
            raise ValueError("Build inputs changed; freeze/apply native patches before building and retry")
    output = args.output.resolve()
    if output.exists(): raise ValueError("Output must be a new directory")
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
    for path, _ in inputs:
        if not path.is_file(): raise FileNotFoundError(path)
    pin_text = (ROOT/"THIRD_PARTY_NOTICES.md").read_text(encoding="utf-8")
    if PIN not in pin_text: raise ValueError("ProtoInput pin changed; audit runtime set")
    if (ROOT/"vendor/ProtoInput/.git").exists() and command("git", "-C", str(ROOT/"vendor/ProtoInput"), "rev-parse", "HEAD") != PIN:
        raise ValueError("ProtoInput checkout does not match audited pin")
    source_names = source_files()
    sources = [{"path": p, "sha256": sha(ROOT/p)} for p in source_names]
    metadata = json.loads(command("cargo", "--config", 'build.rustc-wrapper=""', "metadata", "--locked", "--offline",
                                  "--format-version", "1", "--filter-platform", "x86_64-pc-windows-msvc"))
    nodes = {n["id"] for n in metadata["resolve"]["nodes"]}
    output.mkdir(parents=True)
    for path, relative in inputs:
        target = output/relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
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
    for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
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
    available = {p.name.lower(): p for p in output.rglob("*") if p.suffix.lower() in (".exe", ".dll")}
    for path in available.values():
        report = pe_imports(path)
        expected = "x86" if path.stem.endswith("32") else "x64"
        if report["machine"] != expected: raise ValueError(f"Unexpected architecture: {path}")
        for name in report["imports"] + report["delay_imports"]:
            lower = name.lower()
            if lower in available:
                dependency = available[lower]
                if dependency.parent != path.parent: raise ValueError(f"Dependency outside DLL directory: {path}: {name}")
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
                "expected_rustflags": "-C target-feature=+crt-static", "expected_build_configuration": "Release / x64+x86; Rust release x86_64-pc-windows-msvc --locked",
                "build_inputs_verified": build_inputs is not None, "build_inputs": build_inputs,
                "runtime_files": RUNTIME + ["protoinputmultiplex_cursor_observer32.dll", "protoinputmultiplex_cursor_observer64.dll"], "binaries": binaries, "host_system_imports": sorted(system),
                "dynamic_system_dependencies": ["Windows OpenGL implementation and display driver (GUI)"],
                "rust_dependencies": dependencies, "files": files,
                "validation_scope": "Packaging and launch only; no routing acceptance implied"}
    (output/"bundle-manifest.json").write_text(json.dumps(manifest, indent=2)+"\n", encoding="utf-8")
    print(json.dumps({"bundle": str(output), "files": len(files), "binaries": len(binaries), "rust_notices": len(dependencies)}))


if __name__ == "__main__":
    main()
