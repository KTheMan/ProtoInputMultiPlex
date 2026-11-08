"""Owned registration conflicts and real app-local controls; no physical input."""
import argparse
import copy
import ctypes
from ctypes import wintypes
import hashlib
import importlib.util
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


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def run(args):
    producer = module('startup_producer_guard', 'test-shared-emergency-stop-native.py').freeze(args)
    assert sha(args.x86_executable) == producer['build_sha256']['i686']
    lifecycle = module('startup_lifecycle', 'test-app-lifecycle-ui.py')
    connected = module('startup_connected', 'test-connected-agents-acceptance.py')
    Base, ui, hotkeys, geometry = lifecycle.gate_type()

    class Gate(Base):
        def __init__(self):
            super().__init__(args, 'startup-shortcuts-' + args.phase, offline=False, count=1)
            self.target = None
            self.root = None
            self.layers = set()
            self.holds = {}
            self.originals = {}
            self.expected_profile = None
            self.initial_preferences = None
            self.conflict = None
            self.fixture = hotkeys.OwnedHotkeys()
            self.evidence.update(producer=str(args.producer), producer_sha256=sha(args.producer),
                                 scope='Cold startup shortcut failure only; owned registrations, no routing_input or physical events')

        def once(self, name, operation):
            if name in self.once_results:
                record = self.once_results[name]
                assert record['completed'], 'Failed imperative must not be retried'
                return record['value']
            record = self.once_results[name] = {'completed': False}
            self.evidence.setdefault('owned_actions', {})[name] = record
            self.save()
            result = operation()
            record.update(completed=True, value=result)
            self.save()
            return result

        def scoped_processes(self):
            script = "$p='" + str(self.images[0]).replace("'", "''") + "'; @(Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -eq $p } | Select-Object ProcessId,ParentProcessId,CommandLine,ExecutablePath,CreationDate) | ConvertTo-Json -Compress"
            result = subprocess.run(['powershell', '-NoProfile', '-Command', script], capture_output=True, text=True, timeout=20)
            assert result.returncode == 0
            rows = json.loads(result.stdout) if result.stdout.strip() else []
            return rows if isinstance(rows, list) else [rows]

        def status(self):
            return ui.read_json(self.run / 'ui.app-preferences.shortcuts.json')

        def probes(self):
            return [ui.read_json(self.run / 'probe-0.json')]

        def frame(self, name, size):
            hwnd = self.main_hwnd()
            if geometry.geometry(hwnd)['client_logical'] != list(map(float, size)):
                geometry.resize_owned_client(hwnd, size)
                return False
            started = self.holds.setdefault(name, time.time())
            if time.time() - started < 3.2:
                return False
            result = geometry.copy_owned_product_frame(self.run / 'ui.png', self.run / (name + '.png'), hwnd, started + 2)
            if result:
                self.originals[name] = result
                self.evidence['originals'] = self.originals
            return bool(result)

        def require_inactive(self):
            value = self.status()
            assert value['active'] is False and value['error']
            assert self.preferences_path.read_bytes() == self.initial_preferences
            rows = self.scoped_processes()
            assert not any('session-worker' in (r['CommandLine'] or '') for r in rows)
            assert not any(r.get('overlay', {}).get('hwnd') for r in (ui.read_json(self.run / 'ui.state.json') if (self.run / 'ui.state.json').exists() else []))
            self.evidence.setdefault('inactive_observations', []).append({'status': value, 'processes': rows})
            return value

        def probe_rollback(self):
            if args.phase == 'custom':
                ui.eventually(lambda: self.scoped_processes(), lambda rows: not any('emergency-broker' in (r['CommandLine'] or '') for r in rows), 10)
                return self.fixture.probe(0x7a, True)
            return self.fixture.probe(0x77, True)

        def release_conflict(self):
            self.fixture.unregister(self.conflict['id'])
            return {'released': self.conflict['id']}

        def close_targets(self):
            if self.target and self.target.poll() is None:
                assert geometry.user32.PostMessageW(self.root, 0x10, 0, 0)
                self.target.wait(timeout=10)
            if self.target:
                assert self.target.returncode == 0

    gate = Gate()
    try:
        assert sha(gate.images[0]) == producer['build_sha256']['x64']
        gate.target = subprocess.Popen([str(gate.images[0]), 'probe', '--automation', '--report', str(gate.run / 'probe-0.json')], creationflags=subprocess.CREATE_NO_WINDOW)
        gate.owned.append(gate.target)
        gate.root = ui.eventually(lambda: geometry.owned_window(gate.target.pid), bool, 10)
        ui.eventually(gate.probes, lambda values: values[0]['frames'] > 2, 10)
        assert geometry.user32.SetWindowPos(gate.root, wintypes.HWND(-1), 950, 40, 420, 360, 0x10)
        profile = {'version': 1, 'concurrent': True, 'desktop': None, 'seats': [{'id': 'startup-recovery', 'name': 'Startup recovery', 'mouse': 'virtual://mouse/41', 'keyboard': 'virtual://keyboard/41', 'target_pid': gate.target.pid, 'cursor': {'lock_window': False}}]}
        preferences = lifecycle.preference_defaults()
        preferences.update(launch_minimized=True, minimize_to_tray=False, close_to_tray=False)
        custom = {'action': 'pause_resume', 'key': 'f8', 'modifiers': dict(preferences['shortcuts'][0]['modifiers'])}
        preferences['shortcuts'] = preferences['shortcuts'] + [custom] if args.phase == 'custom' else [custom] + preferences['shortcuts']
        gate.conflict = gate.fixture.register(0x77 if args.phase == 'custom' else 0x7a)
        assert gate.conflict['accepted'], 'Required owned chord is occupied externally; abort without actuating it'
        gate.evidence['foreign_registration'] = gate.conflict
        step = ui.step
        if args.phase == 'custom':
            steps = [step('blocked', 'Start routing', disabled=True), step('retry-failed', 'Retry shortcuts'),
                     step('retry-ok', 'Retry shortcuts'), step('start', 'Start routing'),
                     step('settings', 'Settings'), step('agent-category', 'Settings category Agent access'), step('agents', 'Agent Access'),
                     step('enable-agent', 'Enable agent access', {'agent_users': 'startup-recovery', 'agent_pids': gate.target.pid, 'agent_capture': 'false', 'agent_session_control': 'false'}),
                     step('home', 'Back to Home'), step('stop', 'Stop routing')]
        else:
            steps = [step('blocked', 'Start routing', disabled=True), step('settings', 'Settings'),
                     step('preferences', 'Application preferences'), step('revert', 'Revert application preferences'),
                     step('apply-failed', 'Apply application preferences'),
                     step('apply-corrected', 'Apply application preferences', {'app_shortcut_Stop': 'Ctrl+Alt+Shift+F10'}),
                     step('home', 'Back to Home')]

        def before(_):
            assert gate.initial_preferences is not None
            hwnd = gate.main_hwnd()
            if not hwnd or not gate.lifecycle_hwnd():
                return False
            state = gate.retained_main_state()
            if not state['IsWindowVisible'] or state['IsIconic']:
                return False
            try:
                gate.require_inactive()
            except (OSError, ValueError):
                return False
            gate.evidence['visible_cold_start'] = state
            gate.once('initial-rollback-probe', gate.probe_rollback)
            return gate.frame('inactive-home-normal', [360, 360])

        def verify(name, sample):
            if gate.expected_profile is None:
                gate.expected_profile = copy.deepcopy(sample['profile_state']['profile'])
            assert sample['profile_state']['profile'] == gate.expected_profile
            assert lifecycle.startup_value(gate.startup_key) is None
            sample['shortcut_status'] = gate.status()
            if name == 'blocked':
                gate.require_inactive()
                assert sample['event']['enabled'] is False
                return gate.frame('inactive-home-minimum', [320, 360])
            if name == 'retry-failed':
                if 'Shortcuts unavailable:' not in sample['profile_state']['status']:
                    return False
                gate.require_inactive()
                gate.once('failed-retry-rollback-probe', gate.probe_rollback)
                gate.once('release-owned-conflict', gate.release_conflict)
            elif name == 'retry-ok':
                if not gate.status()['active']:
                    return False
                assert gate.status() == {'active': True, 'error': None}
                assert gate.preferences_path.read_bytes() == gate.initial_preferences
                gate.once('active-default-probe', lambda: gate.fixture.probe(0x7a, False))
                gate.once('active-custom-probe', lambda: gate.fixture.probe(0x77, False))
            elif name == 'start':
                if not sample['state']:
                    return False
                gate.layers = {u['overlay']['hwnd'] for u in sample['state']}
                assert gate.layers and all(ui.user32.IsWindow(h) for h in gate.layers)
                gate.evidence['live_worker'] = [r for r in gate.scoped_processes() if 'session-worker' in (r['CommandLine'] or '')]
                assert len(gate.evidence['live_worker']) == 1
            elif name == 'enable-agent':
                policy = ui.read_json(gate.run / 'ui.agent.json')
                if policy['pending'] or not policy['policy_enabled']:
                    return False
                descriptor = ui.eventually(lambda: list(Path(tempfile.gettempdir()).glob(f'protoinputmultiplex-agent-{gate.gui.pid}-*.json')), lambda paths: len(paths) == 1, 10)[0]
                def inspect():
                    client = connected.ConnectedMcp(gate.images[0], descriptor, gate.run / 'connected.stderr.log')
                    gate.connections.append(client)
                    gate.owned.append(client.process)
                    value = client.tool('app_preferences', {'operation': 'inspect'})
                    assert value['source'] == 'connected_gui' and value['shortcut_status'] == {'active': True, 'error': None}
                    assert value['preferences'] == preferences
                    client.close()
                    gate.connections.remove(client)
                    return value
                sample['connected_inspect'] = gate.once('connected-health-inspect', inspect)
            elif name == 'stop':
                recovery = ui.read_json(gate.run / 'ui.recovery.json')
                if recovery['phase'] != 'stopped' or not recovery['can_start'] or any(ui.user32.IsWindow(h) for h in gate.layers):
                    return False
                assert not any('session-worker' in (r['CommandLine'] or '') for r in gate.scoped_processes())
            elif name in ('preferences', 'revert', 'apply-failed'):
                gate.require_inactive()
                prefs = ui.read_json(gate.run / 'ui.app-preferences.json')
                sample['application_preferences'] = prefs
                if name == 'preferences':
                    return gate.frame('inactive-preferences', [480, 560])
                if name == 'apply-failed':
                    if 'not applied' not in sample['profile_state']['status']:
                        return False
                    assert 'not applied' in sample['profile_state']['status']
                    gate.once('failed-apply-rollback-probe', gate.probe_rollback)
            elif name == 'apply-corrected':
                if not gate.status()['active']:
                    return False
                assert gate.status() == {'active': True, 'error': None}
                expected = copy.deepcopy(preferences)
                # The form writes its canonical action order.
                expected['shortcuts'][1]['key'] = 'f10'
                actual = ui.read_json(gate.preferences_path)
                assert {s['action']: s for s in actual['shortcuts']} == {s['action']: s for s in expected['shortcuts']}
                assert {k: v for k, v in actual.items() if k != 'shortcuts'} == {k: v for k, v in expected.items() if k != 'shortcuts'}
                gate.once('corrected-stop-probe', lambda: gate.fixture.probe(0x79, False))
                gate.once('corrected-custom-probe', lambda: gate.fixture.probe(0x77, False))
                sample['saved_corrected_preferences'] = actual
            return True

        def after(_):
            gate.close_targets()
            gate.expected_exit = True
            assert geometry.user32.PostMessageW(gate.main_hwnd(), 0x10, 0, 0)
            gate.gui.wait(timeout=12)
            assert gate.gui.returncode == 0
            ui.eventually(gate.scoped_processes, lambda rows: not rows, 10)
            gate.evidence['final_chord_probes'] = [gate.fixture.probe(vk, True) for vk in ((0x77, 0x7a) if args.phase == 'custom' else (0x77, 0x79))]
            return True

        archive = gate.run / 'runner-source'
        archive.mkdir()
        for name in (Path(__file__).name, 'test-shared-emergency-stop-native.py', 'test-connected-agents-acceptance.py', 'owned-gui-geometry.py'):
            shutil.copy2(Path(__file__).with_name(name), archive / name)
        ui.put(gate.preferences_path, preferences)
        gate.initial_preferences = gate.preferences_path.read_bytes()
        gate.evidence['initial_preferences_sha256'] = sha(gate.preferences_path)
        gate.execute(profile, preferences, steps, verify, before_controls=before, after_controls=after)
        gate.evidence['result'] = 'PENDING INDEPENDENT DATA/PIXEL REVIEW'
    finally:
        errors = gate.evidence.setdefault('cleanup_errors', [])
        for client in gate.connections:
            try:
                client.close()
            except Exception as error:
                errors.append(repr(error))
        try:
            gate.close_targets()
        except Exception as error:
            errors.append(repr(error))
        gate.close()
        gate.fixture.close()
        lifecycle.remove_startup_key(gate.startup_key)
        gate.evidence['guid_value_absent'] = lifecycle.startup_value(gate.startup_key) is None
        gate.evidence['descriptor_absent'] = gate.gui is None or not list(Path(tempfile.gettempdir()).glob(f'protoinputmultiplex-agent-{gate.gui.pid}-*.json'))
        gate.evidence['known_overlays_closed'] = all(not ui.user32.IsWindow(h) for h in gate.layers)
        gate.evidence['owned_exit_codes'] = {str(p.pid): p.returncode for p in gate.owned}
        try:
            gate.evidence['scoped_cim_survivors'] = ui.eventually(gate.scoped_processes, lambda rows: not rows, 10)
        except Exception as error:
            errors.append(repr(error))
        if errors or any(p.returncode != 0 for p in gate.owned) or not gate.evidence['guid_value_absent'] or not gate.evidence['descriptor_absent'] or not gate.evidence['known_overlays_closed']:
            gate.evidence['result'] = 'NOT ACCEPTED'
        gate.save()
        print(json.dumps({'run': str(gate.run), 'result': gate.evidence['result']}), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--ready', action='store_true')
    for name in ('executable', 'x86-executable', 'runtime', 'producer'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--producer-sha256', required=True)
    parser.add_argument('--phase', choices=['custom', 'default'], required=True)
    run(parser.parse_args())
