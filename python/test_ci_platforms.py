"""bound provisioning and clean up owned volumes without hiding failed tests."""

import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
FAKE_COMMAND = r'''
import json, os, plistlib, signal, sys
from pathlib import Path
root = Path(os.environ['CI_TEST_ROOT'])
settings = json.loads((root / 'settings.json').read_text())
state_path = root / 'state.json'
state = json.loads(state_path.read_text()) if state_path.exists() else {}
name, args = Path(sys.argv[0]).name, sys.argv[1:]
with (root / 'calls.jsonl').open('a') as log:
    log.write(json.dumps([name, *args]) + '\n')
def count(key):
    state[key] = state.get(key, 0) + 1
    state_path.write_text(json.dumps(state))
    return state[key]
if name == 'sudo':
    if args[0] == 'tee':
        assert args[1:] == ['/etc/resolv.conf']
        (root / 'resolver.txt').write_text(sys.stdin.read())
        sys.exit(0)
    assert args[0] in ('apt-get', 'timeout', 'mdutil')
    os.execv(str(root / 'bin' / args[0]), args)
elif name == 'timeout':
    number = count('attempt')
    if settings.get('apt') == 'timeout_once' and number == 1:
        sys.exit(124)
    if settings.get('apt') == 'hang_once':
        os.execv(settings['real_timeout'], ['timeout', *args])
    start = args.index('bash')
    os.execv(settings['bash'], args[start:])
elif name == 'apt-get':
    operation = 'update' if 'update' in args else 'install'
    number = count(operation)
    mode = settings.get('apt')
    if mode == 'hang_once' and operation == 'update' and number == 1:
        signal.pause()
    if mode == 'always_fail' or mode == 'failure_once' and operation == 'update' and number == 1:
        sys.exit(100)
elif name == 'hdiutil':
    mount = root / 'runner/hfs'
    if args[0] == 'attach':
        mount.mkdir()
        sys.stdout.buffer.write(plistlib.dumps({'system-entities': [
            {'dev-entry': '/dev/disk5'},
            {'dev-entry': '/dev/disk5s2', 'mount-point': str(mount)},
        ]}))
    elif args[0] == 'detach':
        number = count('detach')
        if mount.exists():
            mount.rmdir()
        elif args[-1] != '/dev/disk5':
            print('hdiutil: detach failed - No such file or directory', file=sys.stderr)
            sys.exit(1)
        if '-force' in args:
            sys.exit(settings.get('force_exit', 0))
        if number <= settings.get('busy', 0):
            print('hdiutil: could not eject disk5 - Resource busy', file=sys.stderr)
            sys.exit(1)
elif name == 'python3':
    if args[0] == '-':
        os.execv(settings['python'], [settings['python'], *args])
    assert args == ['python/ci_tests.py', 'run', 'hfs', '--disk']
    sys.exit(settings.get('test_exit', 0))
elif name not in ('diskutil', 'mdutil', 'sleep'):
    raise AssertionError(name)
'''


@unittest.skipIf(os.name == 'nt', 'POSIX CI provisioning helpers')
class PlatformScripts(unittest.TestCase):
    def run_script(self, script, **settings):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        (root / 'bin').mkdir()
        (root / 'runner').mkdir()
        settings.update(bash=shutil.which('bash'), python=sys.executable, real_timeout=shutil.which('timeout'))
        (root / 'settings.json').write_text(json.dumps(settings), encoding='utf-8')
        for name in ['sudo', 'apt-get', 'timeout', 'hdiutil', 'diskutil', 'mdutil', 'sleep', 'python3']:
            path = root / 'bin' / name
            path.write_text(f'#!{sys.executable}\n' + FAKE_COMMAND, encoding='utf-8')
            path.chmod(0o755)
        env = dict(os.environ, PATH=str(root / 'bin') + os.pathsep + os.environ['PATH'],
                   CI_TEST_ROOT=str(root), RUNNER_TEMP=str(root / 'runner'), CI_APT_ATTEMPT_TIMEOUT='1s')
        process = subprocess.Popen(['bash', str(ROOT / '.github/scripts' / script), 'fixture-package'],
                                   cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   text=True, start_new_session=True)
        try:
            stdout, stderr = process.communicate(timeout=15)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate()
            self.fail('the provisioning helper exceeded its test guard')
        calls = [json.loads(line) for line in (root / 'calls.jsonl').read_text().splitlines()]
        return process.returncode, stdout + stderr, calls, root

    def test_apt_attempts_are_bounded_and_failures_stay_red(self):
        for mode, attempts, code in [('success', 1, 0), ('timeout_once', 2, 0),
                                     ('failure_once', 2, 0), ('always_fail', 2, 100)]:
            with self.subTest(mode=mode):
                actual, output, calls, root = self.run_script('apt-install.sh', apt=mode)
                self.assertEqual(actual, code, output)
                bounded = [call for call in calls if call[0] == 'timeout']
                self.assertEqual(len(bounded), attempts)
                self.assertTrue(all('1s' in call and '--kill-after=10s' in call for call in bounded))
                self.assertEqual((root / 'resolver.txt').exists(), attempts == 2)
                self.assertIn('apt (', output)
                if mode == 'timeout_once':
                    self.assertIn('timed out', output)

    @unittest.skipUnless(sys.platform.startswith('linux') and shutil.which('timeout'), 'GNU timeout')
    def test_a_real_hung_update_reaches_the_resolver_fallback(self):
        code, output, calls, root = self.run_script('apt-install.sh', apt='hang_once')
        self.assertEqual(code, 0, output)
        self.assertIn('timed out', output)
        self.assertEqual((root / 'resolver.txt').read_text(), 'nameserver 1.1.1.1\nnameserver 8.8.8.8\n')
        self.assertEqual(len([call for call in calls if call[0] == 'timeout']), 2)

    def test_hfs_cleanup_uses_the_device_and_preserves_failures(self):
        for busy, test_exit, force_exit, expected in [(0, 0, 0, 0), (2, 0, 0, 0),
                                                     (99, 0, 0, 0), (99, 19, 0, 19), (99, 0, 1, 1)]:
            with self.subTest(busy=busy, test_exit=test_exit, force_exit=force_exit):
                code, output, calls, _ = self.run_script('macos-filesystem.sh', busy=busy,
                                                        test_exit=test_exit, force_exit=force_exit)
                self.assertEqual(code, expected, output)
                detach = [call for call in calls if call[:2] == ['hdiutil', 'detach']]
                self.assertTrue(detach and all(call[-1] == '/dev/disk5' for call in detach))
                self.assertEqual(len(detach), min(busy + 1, 4))
                self.assertEqual('-force' in detach[-1], busy >= 3)
                indexing = next(i for i, call in enumerate(calls) if call[:3] == ['mdutil', '-i', 'off'])
                testing = next(i for i, call in enumerate(calls) if call[:2] == ['python3', 'python/ci_tests.py'])
                self.assertLess(indexing, testing)
