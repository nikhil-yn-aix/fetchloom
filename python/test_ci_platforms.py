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
import json, os, plistlib, re, signal, subprocess, sys
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
    if args[0] == 'sed':
        # gnu sed's delete in place, as the runner has it and macos's sed spells otherwise
        assert args[1] == '-i' and args[3] == os.environ['CI_APT_MIRRORS'], args
        assert args[2].startswith('/') and args[2].endswith('/d'), args
        listed = Path(args[3])
        lines = listed.read_text().splitlines(keepends=True)
        listed.write_text(''.join(line for line in lines if not re.search(args[2][1:-2], line)))
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
elif name == 'gh':
    # github's runs api as it answered on 2026-11-18: a status filter finds no run of a commit
    # while any run of it is in progress, as main's own push run is when it asks
    if settings.get('gh_fails'):
        print('gh: Bad credentials (HTTP 401)', file=sys.stderr)
        sys.exit(1)
    query = dict(args[i + 1].split('=', 1) for i, arg in enumerate(args) if arg == '-f')
    runs = [run for run in settings['runs'] if run['head_sha'] == query['head_sha']]
    if 'event' in query:
        runs = [run for run in runs if run['event'] == query['event']]
    if 'status' in query:
        unfinished = any(run['status'] != 'completed' for run in settings['runs']
                         if run['head_sha'] == query['head_sha'])
        runs = [] if unfinished else [run for run in runs if query['status'] in (run['status'], run['conclusion'])]
    answer = json.dumps({'total_count': len(runs), 'workflow_runs': runs})
    jq = subprocess.run(['jq', '-r', args[args.index('--jq') + 1]], input=answer, capture_output=True, text=True)
    sys.stdout.write(jq.stdout)
    sys.exit(jq.returncode)
elif name == 'lslocks':
    if count('lslocks') <= settings.get('locked', 0):
        print('42 dpkg /var/lib/dpkg/lock-frontend')
elif name == 'hdiutil':
    mount = root / 'runner' / settings.get('volume', 'hfs')
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
    assert args == ['python/ci_tests.py', 'run', settings.get('volume', 'hfs'), settings.get('scope', '--disk')]
    sys.exit(settings.get('test_exit', 0))
elif name not in ('diskutil', 'mdutil', 'sleep'):
    raise AssertionError(name)
'''


@unittest.skipIf(os.name == 'nt', 'POSIX CI provisioning helpers')
class PlatformScripts(unittest.TestCase):
    def run_script(self, script, attempt='1s', guard=15, arguments=('fixture-package',), **settings):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        (root / 'bin').mkdir()
        (root / 'runner').mkdir()
        settings.update(bash=shutil.which('bash'), python=sys.executable, real_timeout=shutil.which('timeout'))
        (root / 'settings.json').write_text(json.dumps(settings), encoding='utf-8')
        for name in ['sudo', 'apt-get', 'timeout', 'lslocks', 'gh', 'hdiutil', 'diskutil', 'mdutil', 'sleep',
                     'python3']:
            path = root / 'bin' / name
            path.write_text(f'#!{sys.executable}\n' + FAKE_COMMAND, encoding='utf-8')
            path.chmod(0o755)
        if 'mirrors' in settings:
            (root / 'apt-mirrors.txt').write_text(settings['mirrors'], encoding='utf-8')
        env = dict(os.environ, PATH=str(root / 'bin') + os.pathsep + os.environ['PATH'],
                   CI_TEST_ROOT=str(root), RUNNER_TEMP=str(root / 'runner'), CI_APT_ATTEMPT_TIMEOUT=attempt,
                   CI_APT_MIRRORS=str(root / 'apt-mirrors.txt'), GITHUB_OUTPUT=str(root / 'github-output'))
        process = subprocess.Popen(['bash', str(ROOT / '.github/scripts' / script), *arguments],
                                   cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   text=True, start_new_session=True)
        try:
            stdout, stderr = process.communicate(timeout=guard)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate()
            self.fail('the provisioning helper exceeded its test guard')
        log = root / 'calls.jsonl'
        calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
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
        # the hung attempt ends at the limit; the retry that follows must finish inside it, so the
        # limit leaves room for a busy machine to start its four programs
        code, output, calls, root = self.run_script('apt-install.sh', attempt='5s', guard=30, apt='hang_once')
        self.assertEqual(code, 0, output)
        self.assertIn('timed out', output)
        self.assertEqual((root / 'resolver.txt').read_text(), 'nameserver 1.1.1.1\nnameserver 8.8.8.8\n')
        self.assertEqual(len([call for call in calls if call[0] == 'timeout']), 2)

    def test_a_held_lock_is_waited_for_within_the_attempt_and_told(self):
        # the dpkg an attempt cut short leaves running holds dpkg's lock, which apt-get fails on at once
        code, output, calls, _ = self.run_script('apt-install.sh', attempt='5s', apt='success', locked=3)
        self.assertEqual(code, 0, output)
        self.assertIn('apt (runner): waiting up to 5s for /var/lib/dpkg/lock-frontend, held by dpkg (pid 42)\n',
                      output)
        self.assertIn('::notice::apt (runner) waited 2s for /var/lib/dpkg/lock-frontend, held by dpkg (pid 42)\n',
                      output)
        update, install = [call for call in calls if call[0] == 'apt-get']
        self.assertIn('DPkg::Lock::Timeout=5', update)
        self.assertIn('DPkg::Lock::Timeout=3', install)
        self.assertEqual([call[4] for call in calls if call[0] == 'timeout'], ['5s'])

    def test_the_retry_takes_the_mirrors_after_azures(self):
        # azure's mirror can crawl for minutes, which no apt setting fails over
        mirrors = 'http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\nhttps://archive.ubuntu.com/ubuntu/\tpriority:2\n'
        for mode, kept in [('success', mirrors), ('failure_once', 'https://archive.ubuntu.com/ubuntu/\tpriority:2\n')]:
            with self.subTest(mode=mode):
                code, output, _, root = self.run_script('apt-install.sh', apt=mode, mirrors=mirrors)
                self.assertEqual(code, 0, output)
                self.assertEqual((root / 'apt-mirrors.txt').read_text(encoding='utf-8'), kept)

    @unittest.skipUnless(shutil.which('jq'), 'jq, which answers for the fake api')
    def test_a_pull_request_run_that_passed_is_found_while_main_runs_on_the_commit(self):
        sha = 'a' * 40
        runs = [{'event': 'push', 'status': 'in_progress', 'conclusion': None, 'head_sha': sha,
                 'html_url': 'https://example.org/runs/2'},
                {'event': 'pull_request', 'status': 'completed', 'conclusion': 'success', 'head_sha': sha,
                 'html_url': 'https://example.org/runs/1'}]
        code, output, _, root = self.run_script('ci-verified.sh', arguments=['lab/fetchloom', sha], runs=runs)
        self.assertEqual(code, 0, output)
        self.assertEqual((root / 'github-output').read_text(), 'found=true\nrun=https://example.org/runs/1\n', output)

    @unittest.skipUnless(shutil.which('jq'), 'jq, which answers for the fake api')
    def test_no_other_run_proves_a_commit(self):
        sha = 'a' * 40
        def run(**changed):
            return {'event': 'pull_request', 'status': 'completed', 'conclusion': 'success', 'head_sha': sha,
                    'html_url': 'https://example.org/runs/1', **changed}
        for case, runs, settings in [
            ('a dispatched run', [run(event='workflow_dispatch')], {}),
            ('a failed pull request run', [run(conclusion='failure')], {}),
            ('an unfinished pull request run', [run(status='in_progress', conclusion=None)], {}),
            ("another commit's run", [run(head_sha='b' * 40)], {}),
            ('an api that does not answer', [run()], {'gh_fails': True}),
        ]:
            with self.subTest(case=case):
                code, output, _, root = self.run_script('ci-verified.sh', arguments=['lab/fetchloom', sha],
                                                        runs=runs, **settings)
                self.assertEqual(code, 0, output)
                self.assertEqual((root / 'github-output').read_text(), 'found=false\n', output)

    def test_hfs_cleanup_uses_the_device_and_preserves_failures(self):
        for busy, test_exit, force_exit, expected in [(0, 0, 0, 0), (2, 0, 0, 0),
                                                     (99, 0, 0, 0), (99, 19, 0, 19), (99, 0, 1, 1)]:
            with self.subTest(busy=busy, test_exit=test_exit, force_exit=force_exit):
                code, output, calls, _ = self.run_script('macos-filesystem.sh', arguments=['hfs'], busy=busy,
                                                        test_exit=test_exit, force_exit=force_exit)
                self.assertEqual(code, expected, output)
                detach = [call for call in calls if call[:2] == ['hdiutil', 'detach']]
                self.assertTrue(detach and all(call[-1] == '/dev/disk5' for call in detach))
                self.assertEqual(len(detach), min(busy + 1, 4))
                self.assertEqual('-force' in detach[-1], busy >= 3)
                indexing = next(i for i, call in enumerate(calls) if call[:3] == ['mdutil', '-i', 'off'])
                testing = next(i for i, call in enumerate(calls) if call[:2] == ['python3', 'python/ci_tests.py'])
                self.assertLess(indexing, testing)

    def test_exfat_runs_the_placing_tests_on_a_small_image(self):
        code, output, calls, _ = self.run_script('macos-filesystem.sh', arguments=['exfat'], volume='exfat',
                                                scope='--placing')
        self.assertEqual(code, 0, output)
        create = next(call for call in calls if call[:2] == ['hdiutil', 'create'])
        self.assertEqual(create[create.index('-fs') + 1], 'ExFAT')
        self.assertEqual(create[create.index('-size') + 1], '64m')
        self.assertIn(['python3', 'python/ci_tests.py', 'run', 'exfat', '--placing'], calls)

    def test_a_filesystem_without_a_recipe_is_refused(self):
        code, output, calls, _ = self.run_script('macos-filesystem.sh', arguments=['zfs'])
        self.assertEqual(code, 2, output)
        self.assertIn('no disk image recipe for zfs', output)
        self.assertFalse([call for call in calls if call[0] == 'hdiutil'])
