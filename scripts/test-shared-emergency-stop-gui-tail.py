"""Owned GUI + two standalone controllers: shared emergency loss and explicit retry.

Preparation only until --ready binds the reviewed producer. No OS input injection.
"""
import argparse
import copy
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def run(args):
    native = module('shared_stop_native_guard', 'test-shared-emergency-stop-native.py')
    producer = native.freeze(args)
    assert args.phase == 'gui-tail', 'Accepted functional prefix must not be replayed; use the guarded GUI tail'
    root = Path(__file__).resolve().parent.parent
    assert args.from_run.resolve() == (root / 'runtime/acceptance-graph-shared-stop-gui-20261015-105728').resolve()
    assert digest(args.from_run / 'evidence.json') == '39307e5afa70d6c87caac0dad09eb58bcd1a595f225e951aba9fde431a355ce7'
    assert args.prefix_review.resolve().parent == args.from_run.resolve()
    assert digest(args.prefix_review) == args.prefix_review_sha256.lower() == 'e8383298ca8d5df54f2c81ddffbdd8dbd1e2c78d9222364bbfc7480175a98762'
    prior = json.loads((args.from_run / 'evidence.json').read_text(encoding='utf-8'))
    assert prior['producer_sha256'] == args.producer_sha256.lower()
    review = json.loads(args.prefix_review.read_text(encoding='utf-8'))
    assert review['decision'] == 'BROKER-LOSS FUNCTIONAL PREFIX APPROVED; GUI AGGREGATE AND PIXELS NOT ACCEPTED'
    assert digest(args.x86_executable) == producer['build_sha256']['i686']
    lifecycle = module('shared_stop_gui_base', 'test-app-lifecycle-ui.py')
    mcp = module('shared_stop_gui_mcp', 'test-mcp.py')
    Base, ui, hotkeys, geometry = lifecycle.gate_type()
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel.WaitForSingleObject.restype = wintypes.DWORD
    kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
    kernel.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    kernel.QueryFullProcessImageNameW.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD)]
    geometry.user32.FindWindowExW.argtypes = [wintypes.HWND, wintypes.HWND, wintypes.LPCWSTR, wintypes.LPCWSTR]
    geometry.user32.FindWindowExW.restype = wintypes.HWND
    geometry.user32.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]

    class Gate(Base):
        def __init__(self):
            super().__init__(args, 'shared-stop-gui-tail', offline=False, count=1)
            self.target = None
            self.root = None
            self.clients = []
            self.extra_targets = []
            self.layers = set()
            self.watch = []
            self.holds = {}
            self.originals = {}
            self.expected_profile = None

        def once(self, name, operation):
            if name in self.once_results:
                result = self.once_results[name]
                assert result['completed'], 'A prior imperative attempt failed; no replay allowed'
                return result['value']
            self.once_results[name] = {'completed': False}
            try:
                value = operation()
            except Exception as error:
                raise RuntimeError('Owned imperative failed; no replay allowed: ' + name) from error
            self.once_results[name] = {'completed': True, 'value': value}
            self.evidence.setdefault('external_owned_actions', {})[name] = value
            self.save()
            return value

        def probes(self):
            return ui.eventually(lambda: [ui.read_json(self.run / 'probe-0.json')], lambda _: True, 3)

        def snapshot(self):
            return ui.eventually(lambda: super(Gate, self).snapshot(), lambda _: True, 3)

        def frame(self, name, size):
            hwnd = self.main_hwnd()
            if geometry.geometry(hwnd)['client_logical'] != list(map(float, size)):
                geometry.resize_owned_client(hwnd, size)
                return False
            hold = self.holds.setdefault(name, {'started': time.time(), 'writes': []})
            grace = hold['started'] + 3.2
            if time.time() < grace:
                return False
            try:
                from PIL import Image
                source = self.run / 'ui.png'
                stat = source.stat()
                if stat.st_mtime <= grace:
                    return False
                raw = source.read_bytes()
                picture = Image.open(io.BytesIO(raw))
                picture.load()
                if picture.size != tuple(size) or source.stat().st_mtime_ns != stat.st_mtime_ns:
                    return False
                observed = [stat.st_mtime_ns, len(raw), hashlib.sha256(raw).hexdigest()]
                if not hold['writes'] or observed[0] != hold['writes'][-1][0]:
                    hold['writes'].append(observed)
                if len(hold['writes']) < 2:
                    return False
                original = geometry.copy_owned_product_frame(source, self.run / (name + '.png'), hwnd, grace)
                if not original:
                    return False
                self.originals[name] = original
                self.evidence['originals'] = self.originals
                self.evidence['capture_holds'] = self.holds
                return True
            except (OSError, ValueError):
                return False

        def call(self, client, name, arguments=None):
            mutating = name not in ('routing_state', 'routing_windows')
            calls = self.evidence.setdefault('mcp_calls', [])
            item = {'controller': client.process.pid, 'tool': name, 'arguments': arguments or {}, 'mutating': mutating}
            calls.append(item)
            assert sum(c['mutating'] for c in calls) <= 24
            self.save()
            try:
                item['result'] = client.tool(name, arguments)
            except (OSError, ValueError) as error:
                raise RuntimeError('Imperative request must not be retried') from error
            self.save()
            return item['result']

        def scoped_processes(self):
            # Exact private run image only; this inventory cannot target other applications.
            script = "$p='" + str(self.images[0]).replace("'", "''") + "'; @(Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -eq $p } | Select-Object ProcessId,ParentProcessId,CommandLine,ExecutablePath,CreationDate) | ConvertTo-Json -Compress"
            result = subprocess.run(['powershell', '-NoProfile', '-Command', script], capture_output=True, text=True, timeout=20)
            assert result.returncode == 0
            rows = json.loads(result.stdout) if result.stdout.strip() else []
            return rows if isinstance(rows, list) else [rows]

        def retain(self, row, kind):
            pid = int(row['ProcessId'])
            handle = kernel.OpenProcess(0x100000 | 0x1000 | (1 if kind == 'broker' else 0), False, pid)
            assert handle
            item = {'kind': kind, **row}
            self.watch.append((handle, item))
            self.evidence.setdefault('watched_processes', []).append(item)
            assert kernel.WaitForSingleObject(handle, 0) == 258
            times = [wintypes.FILETIME() for _ in range(4)]
            assert kernel.GetProcessTimes(handle, *(ctypes.byref(v) for v in times))
            creation = (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime
            path = ctypes.create_unicode_buffer(32768)
            length = wintypes.DWORD(len(path))
            assert kernel.QueryFullProcessImageNameW(handle, 0, path, ctypes.byref(length))
            assert Path(path.value).resolve() == self.images[0].resolve()
            current = next(r for r in self.scoped_processes() if int(r['ProcessId']) == pid)
            assert current['CommandLine'] == row['CommandLine'] and current['CreationDate'] == row['CreationDate']
            assert kernel.WaitForSingleObject(handle, 0) == 258
            item.update(creation_filetime=creation, handle_image=path.value)
            if kind.startswith('broker'):
                assert 'emergency-broker' in current['CommandLine']
                matches = []
                child = None
                seen = set()
                while True:
                    child = geometry.user32.FindWindowExW(wintypes.HWND(-3), child, None, None)
                    if not child:
                        break
                    assert int(child) not in seen and len(seen) < 10000
                    seen.add(int(child))
                    owner = wintypes.DWORD()
                    assert geometry.user32.GetWindowThreadProcessId(child, ctypes.byref(owner))
                    if owner.value == pid:
                        class_name = ctypes.create_unicode_buffer(256)
                        assert geometry.user32.GetClassNameW(child, class_name, len(class_name))
                        if class_name.value.startswith('ProtoInputMultiPlex.EmergencyStop.v1.'):
                            matches.append({'hwnd': int(child), 'class': class_name.value, 'pid': owner.value})
                assert len(matches) == 1
                item['owned_broker_window'] = matches[0]
            return handle, item

        def prepare_peers(self):
            for index in range(2):
                path = self.run / f'peer-{index}.json'
                process = subprocess.Popen([str(self.images[0]), 'probe', '--automation', '--report', str(path)], creationflags=subprocess.CREATE_NO_WINDOW)
                self.owned.append(process)
                root = ui.eventually(lambda: geometry.owned_window(process.pid), bool, 10)
                report = ui.eventually(lambda: ui.read_json(path), lambda r: r['frames'] > 2, 10)
                assert geometry.user32.SetWindowPos(root, wintypes.HWND(-1), 30 + index * 460, 40, 420, 360, 0x10)
                target = {'process': process, 'root': root, 'path': path, 'child': report['capture_hwnd'], 'key': 'F6' if index == 0 else 'F7', 'vk': 0x75 + index}
                self.extra_targets.append(target)
                client = mcp.Client(self.images[0], args.runtime.resolve(), self.run / f'peer-{index}.stderr.log')
                self.clients.append(client)
                self.owned.append(client.process)
                self.call(client, 'routing_start', {'users': [f'Peer {index + 1}'], 'allowed_pids': [process.pid], 'mode': 'concurrent', 'multi_keyboard': True})
                state = ui.eventually(lambda: self.call(client, 'routing_state'), lambda s: len(s['users']) == 1, 10)
                overlays = {u['overlay']['hwnd'] for u in state['users']}
                assert len(overlays) == 1 and all(h and ui.user32.IsWindow(h) for h in overlays)
                self.layers.update(overlays)
                settings = copy.deepcopy(state['user_settings'][0]['settings'])
                settings['keyboard'] = {'enabled': True, 'focus': 'clicked'}
                self.call(client, 'routing_settings', {'user': 0, 'settings': settings})
                rect = geometry.Rect()
                assert geometry.user32.GetWindowRect(target['child'], ctypes.byref(rect))
                assert rect.left <= rect.left + 70 < rect.right and rect.top <= rect.top + 60 < rect.bottom
                self.call(client, 'routing_input', {'action': 'move_to', 'user': 0, 'x': rect.left + 70, 'y': rect.top + 60})
                for pressed in (True, False):
                    self.call(client, 'routing_input', {'action': 'button', 'user': 0, 'button': 'primary', 'pressed': pressed})
                selected = ui.eventually(lambda: self.call(client, 'routing_state'), lambda s: s['users'][0]['keyboard']['clicked'] and s['users'][0]['keyboard']['clicked']['hwnd'] == target['child'], 10)
                assert selected['users'][0]['pid'] == process.pid
                self.call(client, 'routing_input', {'action': 'key', 'user': 0, 'key': target['key'], 'pressed': True})
                ui.eventually(lambda: ui.read_json(path), lambda r: any(e['kind'] == 'key' and e['code'] == target['vk'] and e['pressed'] for e in r['native_events']), 8)
            return True

        def kill_owned_broker(self):
            rows = self.scoped_processes()
            brokers = [r for r in rows if 'emergency-broker' in (r['CommandLine'] or '')]
            assert len(brokers) == 1, brokers
            workers = [r for r in rows if 'session-worker' in (r['CommandLine'] or '')]
            assert len(workers) == 1 and workers[0]['ParentProcessId'] == self.gui.pid, workers
            for row in workers:
                self.retain(row, 'worker')
            handle, item = self.retain(brokers[0], 'broker')
            self.evidence['gui_tail_setup'] = {'gui': self.gui.pid, 'standalone': [], 'broker': brokers[0], 'workers': workers}
            self.save()
            owner = wintypes.DWORD()
            assert geometry.user32.GetWindowThreadProcessId(item['owned_broker_window']['hwnd'], ctypes.byref(owner))
            assert owner.value == item['ProcessId'] and kernel.WaitForSingleObject(handle, 0) == 258
            assert kernel.TerminateProcess(handle, 23)
            assert kernel.WaitForSingleObject(handle, 8000) == 0
            code = wintypes.DWORD()
            assert kernel.GetExitCodeProcess(handle, ctypes.byref(code))
            assert code.value == 23
            item['deliberate_exit_code'] = code.value
            return item

        def losses_stopped(self):
            for client, target in zip(self.clients, self.extra_targets):
                value = ui.eventually(lambda: self.call(client, 'routing_state'), lambda s: not s['running'], 15)
                assert value['last_error'] and 'Emergency stop unavailable' in value['last_error']
                assert value['recovery']['phase'] == 'stopped' and value['recovery']['can_start']
                report = ui.eventually(lambda: ui.read_json(target['path']), lambda r: [e['pressed'] for e in r['native_events'] if e['kind'] == 'key' and e['code'] == target['vk']] == [True, False], 10)
                trace = [e for e in report['native_events'] if e['kind'] == 'key' and e['code'] == target['vk']]
                assert [e['pressed'] for e in trace] == [True, False]
                assert all(e['hwnd'] == target['child'] for e in trace)
                self.evidence.setdefault('loss_stops', []).append({'state': value, 'trace': trace})
            for handle, item in self.watch:
                assert kernel.WaitForSingleObject(handle, 10000) == 0
                code = wintypes.DWORD()
                assert kernel.GetExitCodeProcess(handle, ctypes.byref(code))
                item['observed_exit_code'] = code.value
                assert code.value == (23 if item['kind'] == 'broker' else 0)
            ui.eventually(lambda: [ui.user32.IsWindow(h) for h in self.layers], lambda values: not any(values), 12)
            return True

        def close_targets(self):
            targets = list(self.extra_targets)
            if self.target:
                targets.append({'process': self.target, 'root': self.root})
            for target in targets:
                process = target['process']
                if process.poll() is None:
                    assert geometry.user32.PostMessageW(target['root'], 0x10, 0, 0)
                    process.wait(timeout=10)
                assert process.returncode == 0

    gate = Gate()
    try:
        gate.target = subprocess.Popen([str(gate.images[0]), 'probe', '--automation', '--report', str(gate.run / 'probe-0.json')], creationflags=subprocess.CREATE_NO_WINDOW)
        gate.owned.append(gate.target)
        gate.root = ui.eventually(lambda: geometry.owned_window(gate.target.pid), bool, 10)
        ui.eventually(gate.probes, lambda rs: rs[0]['frames'] > 2, 10)
        assert geometry.user32.SetWindowPos(gate.root, wintypes.HWND(-1), 950, 40, 420, 360, 0x10)
        profile = {'version': 1, 'concurrent': True, 'desktop': None, 'seats': [{'id': 'shared-stop-gui', 'name': 'Shared Stop', 'mouse': 'virtual://mouse/41', 'keyboard': 'virtual://keyboard/41', 'target_pid': gate.target.pid, 'cursor': {'lock_window': False}}]}
        preferences = lifecycle.preference_defaults()
        preferences.update(launch_minimized=False, minimize_to_tray=False, close_to_tray=False)
        steps = [ui.step('start1', 'Start routing'), ui.step('blocked', 'Start routing', disabled=True), ui.step('retry', 'Retry emergency stop'), ui.step('start2', 'Start routing'), ui.step('stop2', 'Stop routing')]

        def verify(name, sample):
            assert ui.read_json(gate.preferences_path) == preferences
            if gate.expected_profile is None:
                gate.expected_profile = copy.deepcopy(sample['profile_state']['profile'])
            assert sample['profile_state']['profile'] == gate.expected_profile
            if name == 'start1':
                if 'kill-owned-broker' not in gate.once_results:
                    if not sample['state']:
                        return False
                    overlays = {u['overlay']['hwnd'] for u in sample['state']}
                    assert len(overlays) == 1 and all(h and ui.user32.IsWindow(h) for h in overlays)
                    gate.layers.update(overlays)
                gate.once('kill-owned-broker', gate.kill_owned_broker)
                gate.once('ordinary-peer-cleanup', gate.losses_stopped)
                recovery = ui.eventually(lambda: ui.read_json(gate.run / 'ui.recovery.json'), lambda r: r['phase'] == 'stopped', 15)
                assert recovery['can_start']
                gate.evidence['gui_loss_recovery'] = recovery
                return gate.frame('unavailable-normal', [360, 360])
            if name == 'blocked':
                assert sample['event']['enabled'] is False
                return gate.frame('unavailable-minimum', [320, 360])
            if name == 'retry':
                brokers = [r for r in gate.scoped_processes() if 'emergency-broker' in (r['CommandLine'] or '')]
                assert len(brokers) == 1
                def retain_retry():
                    _, row = gate.retain(brokers[0], 'broker_retry')
                    old = gate.once_results['kill-owned-broker']['value']
                    assert (row['ProcessId'], row['creation_filetime']) != (old['ProcessId'], old['creation_filetime'])
                    return row
                gate.evidence['retry_broker'] = gate.once('retain-retry-broker', retain_retry)
                return True
            if name == 'start2':
                if not sample['state']:
                    return False
                overlays = {u['overlay']['hwnd'] for u in sample['state']}
                assert len(overlays) == 1 and all(h and ui.user32.IsWindow(h) for h in overlays)
                gate.layers.update(overlays)
                def retain_restart_worker():
                    workers = [r for r in gate.scoped_processes() if 'session-worker' in (r['CommandLine'] or '')]
                    assert len(workers) == 1 and workers[0]['ParentProcessId'] == gate.gui.pid
                    _, row = gate.retain(workers[0], 'worker_restart')
                    return row
                gate.once('retain-restart-worker', retain_restart_worker)
                return True
            recovery = ui.read_json(gate.run / 'ui.recovery.json')
            return recovery['phase'] == 'stopped' and recovery['can_start'] and all(not ui.user32.IsWindow(h) for h in gate.layers)

        def after(_):
            for client in gate.clients:
                client.close()
            gate.close_targets()
            gate.expected_exit = True
            assert geometry.user32.PostMessageW(gate.main_hwnd(), 0x10, 0, 0)
            gate.gui.wait(timeout=12)
            assert gate.gui.returncode == 0
            ui.eventually(gate.scoped_processes, lambda rows: not rows, 10)
            for handle, item in gate.watch:
                assert kernel.WaitForSingleObject(handle, 10000) == 0
                code = wintypes.DWORD()
                assert kernel.GetExitCodeProcess(handle, ctypes.byref(code))
                item['observed_exit_code'] = code.value
                assert code.value == (23 if item['kind'] == 'broker' else 0)
            return True

        gate.evidence.update(producer=str(args.producer), producer_sha256=digest(args.producer), executable_sha256=digest(gate.images[0]), scope='Missing-only GUI error views, disabled Start, explicit retry and restart; no MCP actions or repeated three-controller group', retained_functional_prefix={'path': str(args.from_run), 'raw_sha256': digest(args.from_run / 'evidence.json'), 'review': str(args.prefix_review), 'review_sha256': digest(args.prefix_review)})
        assert digest(gate.images[0]) == producer['build_sha256']['x64']
        archive = gate.run / 'runner-source'
        archive.mkdir(exist_ok=True)
        for name in (Path(__file__).name, 'test-shared-emergency-stop-native.py', 'test-mcp.py', 'owned-gui-geometry.py'):
            shutil.copy2(Path(__file__).with_name(name), archive / name)
        gate.execute(profile, preferences, steps, verify, after_controls=after)
        gate.evidence['result'] = 'PENDING INDEPENDENT DATA/PIXEL REVIEW'
    finally:
        errors = gate.evidence.setdefault('cleanup_errors', [])
        for client in gate.clients:
            try:
                client.close()
            except Exception as error:
                errors.append(repr(error))
        try:
            gate.close_targets()
        except Exception as error:
            errors.append(repr(error))
        gate.close()
        lifecycle.remove_startup_key(gate.startup_key)
        gate.evidence['guid_value_absent'] = lifecycle.startup_value(gate.startup_key) is None
        gate.evidence['descriptor_absent'] = gate.gui is None or not list(Path(tempfile.gettempdir()).glob(f'protoinputmultiplex-agent-{gate.gui.pid}-*.json'))
        gate.evidence['known_overlays_closed'] = bool(gate.layers) and all(not ui.user32.IsWindow(h) for h in gate.layers)
        gate.evidence['owned_exit_codes'] = {str(p.pid): p.returncode for p in gate.owned}
        try:
            gate.evidence['scoped_cim_survivors'] = ui.eventually(gate.scoped_processes, lambda rows: not rows, 10)
        except Exception as error:
            errors.append(repr(error))
        for handle, item in gate.watch:
            kernel.CloseHandle(handle)
        if errors or any(p.returncode != 0 for p in gate.owned) or not gate.evidence['guid_value_absent'] or not gate.evidence['descriptor_absent'] or not gate.evidence['known_overlays_closed']:
            gate.evidence['result'] = 'NOT ACCEPTED'
        gate.save()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--ready', action='store_true')
    for name in ('executable', 'x86-executable', 'runtime', 'producer'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--producer-sha256', required=True)
    parser.add_argument('--phase', choices=['gui-tail'], required=True)
    parser.add_argument('--from-run', type=Path, required=True)
    parser.add_argument('--prefix-review', type=Path, required=True)
    parser.add_argument('--prefix-review-sha256', required=True)
    args = parser.parse_args()
    run(args)


if __name__ == '__main__':
    main()
