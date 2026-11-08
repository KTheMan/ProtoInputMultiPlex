"""Inert manifest adapter for the unchanged frozen package-portable.py.
No Git discovery, native/vendor patching or compiler execution on import.
Root's reviewed release driver invokes this only after RELEASE_SOURCE_GO.
"""
import argparse
import hashlib
import json
from pathlib import Path
import sys
import os


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def exact_source(source,expected):
    assert not source.is_symlink() and not source.is_junction()
    assert not (source/'.cargo').exists()
    actual={};folded=set()
    for base,dirs,names in os.walk(source):
        for name in [*dirs,*names]:
            path=Path(base)/name
            assert not path.is_symlink() and not path.is_junction(),'Source reparse path'
        for name in names:
            path=Path(base)/name;relative=path.relative_to(source).as_posix()
            assert relative.casefold() not in folded
            folded.add(relative.casefold());actual[relative]=sha(path)
    assert actual==expected,'Exact source tree changed (extra/missing/hash/path)'


def cargo_configuration(context):
    settings=context['cargo_configuration'];actual={}
    assert os.environ.get('CARGO_HOME')==settings['cargo_home']
    for name in settings['paths']:
        path=Path(name);assert not path.is_symlink() and not path.is_junction()
        actual[name]=sha(path) if path.is_file() else None
    assert actual==settings['files'],'Cargo configuration changed'


def run(args):
    source=args.source.resolve(strict=True)
    context_path=args.context.resolve(strict=True)
    context=json.loads(context_path.read_bytes())
    script=source/'scripts/package-portable.py'
    assert sha(script)==context['raw_packager_sha256'],'Raw packager changed'
    baseline_path=source/'source-baseline.json'
    assert sha(baseline_path)==context['source_baseline_sha256']
    baseline=json.loads(baseline_path.read_bytes())
    files=baseline['files']
    expected={**files,'source-baseline.json':context['source_baseline_sha256']}
    exact_source(source,expected);cargo_configuration(context)
    text=script.read_text(encoding='utf-8')
    old='native = ROOT/"vendor/ProtoInput/src/ProtoInput/Release"'
    assert text.count(old)==1,'Unrecognized native-input assignment'
    text=text.replace(old,'native = Path(_release_context["backend_dir"])')
    old_flag='"expected_rustflags": "-C target-feature=+crt-static"'
    assert text.count(old_flag)==2,'Unrecognized Rust flag metadata'
    text=text.replace(old_flag,'"expected_rustflags": _release_context["effective_rustflags"]')
    namespace={'__file__':str(script),'__name__':'manifest_packager','_release_context':context}
    exec(compile(text,str(script),'exec'),namespace)
    original_command=namespace['command']
    def command(*parts):
        if parts==('git','status','--short'):
            return 'Manifest-backed immutable source; no Git workspace or commit created'
        if parts and parts[0]=='git':
            raise RuntimeError('Git is outside this manifest-backed packaging operation')
        exact_source(source,expected);cargo_configuration(context)
        result=original_command(*parts)
        exact_source(source,expected);cargo_configuration(context)
        return result
    def source_files():
        return sorted([*files,'source-baseline.json'])
    def native_tools():
        # Reference actual retained receipts instead of inventing a VS/MSBuild
        # installation/version for the current bootstrapped Rust toolchain.
        return {'kind':'accepted_native_backend_reuse_with_fresh_Rust_release_build',
                'backend_qualification':context['backend_qualification'],
                'backend_source_go':context['backend_source_go'],
                'rust_toolchain_environment':context['toolchain_environment'],
                'legacy_native_rebuild_performed':False}
    namespace.update(command=command,source_files=source_files,source_commit=lambda:None,native_tools=native_tools)
    previous=sys.argv
    try:
        sys.argv=[str(script),*args.packager_args]
        namespace['main']()
    finally:
        sys.argv=previous
    exact_source(source,expected);cargo_configuration(context)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source',type=Path,required=True)
    parser.add_argument('--context',type=Path,required=True)
    parser.add_argument('packager_args',nargs=argparse.REMAINDER)
    args=parser.parse_args()
    if args.packager_args[:1]==['--']:args.packager_args=args.packager_args[1:]
    run(args)
