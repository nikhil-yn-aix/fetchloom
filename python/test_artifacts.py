"""artifact gates reject missing targets, unexecuted checks and wrong installed bytes."""

import hashlib
import io
import json
from pathlib import Path
import tarfile
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import urllib.request
import zipfile

from check_archive import check_machine, unpack
from check_artifacts import TARGETS, archive_name, inventory, stages
import check_wheel
from check_wheel import inspect_wheel
from ci_journey import Server, check_version, wheel_version

VERSION = '0.1.0'
COMMIT = 'a' * 40


class InventoryTests(unittest.TestCase):
    def make(self, root, version=VERSION, wheel_version=VERSION):
        packages, proofs = root / 'dist', root / 'proofs'
        packages.mkdir()
        proofs.mkdir()
        for target, metadata in TARGETS.items():
            wheel = f'fetchloom-{wheel_version}-py3-none-' + '.'.join(sorted(metadata['wheel'])) + '.whl'
            names = [archive_name(target, version), wheel]
            if 'deb' in metadata:
                names.append(f"fetchloom_{version}_{metadata['deb']}.deb")
            files = {}
            for name in names:
                data = name.encode()
                (packages / name).write_bytes(data)
                files[name] = hashlib.sha256(data).hexdigest()
            current = '3.14.7' if target == 'x86_64-apple-darwin' else '3.15.2'
            checks = {name: {'ok': True, 'version': version, 'commit': COMMIT,
                             'binary_sha256': 'b' * 64, 'helper': name.startswith('wheel-'),
                             'python': '3.10.11' if name == 'wheel-minimum' else current}
                      for name in stages(target)}
            folder = proofs / target
            folder.mkdir()
            (folder / 'verified.json').write_text(json.dumps({'target': target, 'version': version,
                 'commit': COMMIT, 'checks': checks, 'files': files}), encoding='utf-8')
        (proofs / 'source-package.json').write_text(json.dumps({'ok': True, 'version': version, 'commit': COMMIT}), encoding='utf-8')
        return packages, proofs

    def test_exact_six_target_set_is_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            packages, proofs = self.make(Path(directory))
            self.assertEqual(len(inventory(packages, proofs, VERSION, COMMIT)), 14)

    def test_a_pre_release_wheel_is_named_as_python_spells_its_version(self):
        with tempfile.TemporaryDirectory() as directory:
            packages, proofs = self.make(Path(directory), '0.1.0-rc.1', '0.1.0rc1')
            self.assertEqual(len(inventory(packages, proofs, '0.1.0-rc.1', COMMIT)), 14)

    def test_missing_target_check_asset_or_source_proof_is_rejected(self):
        for change in ['target', 'check', 'asset', 'source', 'checksum', 'revision', 'helper', 'python']:
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                packages, proofs = self.make(Path(directory))
                proof = next(proofs.rglob('verified.json'))
                document = json.loads(proof.read_text())
                if change == 'target':
                    proof.unlink()
                elif change == 'check':
                    document['checks'].pop('archive')
                    proof.write_text(json.dumps(document))
                elif change == 'asset':
                    next(packages.iterdir()).unlink()
                elif change == 'source':
                    (proofs / 'source-package.json').unlink()
                elif change == 'checksum':
                    next(packages.iterdir()).write_bytes(b'wrong')
                elif change == 'helper':
                    document['checks']['wheel-current']['helper'] = False
                    proof.write_text(json.dumps(document))
                elif change == 'python':
                    document['checks']['wheel-current']['python'] = '3.13.9'
                    proof.write_text(json.dumps(document))
                else:
                    document['commit'] = 'c' * 40
                    proof.write_text(json.dumps(document))
                with self.assertRaises((AssertionError, KeyError)):
                    inventory(packages, proofs, VERSION, COMMIT)


class PackageContentTests(unittest.TestCase):
    def test_tar_links_and_extra_entries_are_refused(self):
        for link in [True, False]:
            with self.subTest(link=link), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                path = root / 'bad.tar.gz'
                with tarfile.open(path, 'w:gz') as archive:
                    member = tarfile.TarInfo('../outside' if not link else 'fetchloom-0.1.0-x86_64-unknown-linux-musl/fetchloom')
                    if link:
                        member.type = tarfile.SYMTYPE
                        member.linkname = '../outside'
                    archive.addfile(member)
                with self.assertRaises(AssertionError):
                    unpack(path, 'x86_64-unknown-linux-musl', VERSION, root / 'unpacked')
                self.assertFalse((root / 'outside').exists())

    def test_machine_check_rejects_emulated_wrong_architecture(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / 'fetchloom'
            body = bytearray(64)
            body[:6] = b'\x7fELF\x02\x01'
            body[18:20] = (62).to_bytes(2, 'little')
            binary.write_bytes(body)
            check_machine(binary, 'x86_64-unknown-linux-musl')
            with self.assertRaises(AssertionError):
                check_machine(binary, 'aarch64-unknown-linux-musl')

    def test_wheel_must_bundle_the_tested_binary_and_typed_helper(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'fetchloom'
            binary.write_bytes(b'checked binary')
            for wrong in [False, True]:
                wheel = root / 'package.whl'
                with zipfile.ZipFile(wheel, 'w') as archive:
                    archive.writestr('fetchloom-0.1.0.dist-info/METADATA', 'Name: fetchloom\nVersion: 0.1.0\nRequires-Python: >=3.10\n')
                    archive.writestr('fetchloom/__init__.py', '')
                    archive.writestr('fetchloom/py.typed', '')
                    archive.writestr('fetchloom-0.1.0.data/scripts/fetchloom', b'other binary' if wrong else binary.read_bytes())
                if wrong:
                    with self.assertRaises(AssertionError):
                        inspect_wheel(wheel, VERSION, binary)
                else:
                    inspect_wheel(wheel, VERSION, binary)


    def test_a_pre_release_wheel_records_python_spelling_of_its_version(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'fetchloom'
            binary.write_bytes(b'checked binary')
            wheel = root / 'package.whl'
            with zipfile.ZipFile(wheel, 'w') as archive:
                archive.writestr('fetchloom-0.1.0rc1.dist-info/METADATA', 'Name: fetchloom\nVersion: 0.1.0rc1\nRequires-Python: >=3.10\n')
                archive.writestr('fetchloom/__init__.py', '')
                archive.writestr('fetchloom/py.typed', '')
                archive.writestr('fetchloom-0.1.0rc1.data/scripts/fetchloom', binary.read_bytes())
            inspect_wheel(wheel, '0.1.0-rc.1', binary)


class VersionTests(unittest.TestCase):
    def test_version_prefix_does_not_accept_another_version(self):
        with patch('ci_journey.subprocess.check_output', return_value='fetchloom 0.1.01\n'):
            with self.assertRaises(AssertionError):
                check_version(Path('fetchloom'), VERSION)

    def test_a_wheel_spells_a_pre_release_as_pep_440_does(self):
        for cargo, python in [('0.1.0', '0.1.0'), ('0.1.0-rc.1', '0.1.0rc1'),
                              ('1.2.3-alpha.4', '1.2.3a4'), ('1.2.3-beta.10', '1.2.3b10')]:
            with self.subTest(cargo=cargo):
                self.assertEqual(wheel_version(cargo), python)
        for other in ['0.1.0-dev.1', '0.1.0-rc', '0.1.0+build.1']:
            with self.subTest(other=other), self.assertRaises(AssertionError):
                wheel_version(other)

    def test_python_module_entrypoint_keeps_its_interpreter_arguments(self):
        command = [Path('venv/python'), '-I', '-m', 'fetchloom']
        with patch('ci_journey.subprocess.check_output', return_value='fetchloom 0.1.0\n') as run:
            check_version(command, VERSION)
        self.assertEqual(run.call_args.args[0], [str(Path('venv/python')), '-I', '-m', 'fetchloom', '--version'])


class LoopbackServerTests(unittest.TestCase):
    def test_serving_needs_no_name_lookup(self):
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with patch('socket.getfqdn', side_effect=AssertionError('the server looked up a name')), \
             Server({'/file': b'body'}) as server, opener.open(server.url('/file'), timeout=10) as response:
            self.assertEqual(response.read(), b'body')


class WheelEnvironmentTests(unittest.TestCase):
    def test_restored_rust_cache_does_not_supply_the_wheel_environment(self):
        class EnvironmentSelected(Exception):
            pass

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cached = root / 'target/wheel-check/wheel-current'
            cached.mkdir(parents=True)
            (cached / 'old-install').write_text('previous revision', encoding='utf-8')
            arguments = ['check_wheel.py', 'package.whl', '--binary', 'fetchloom',
                         '--version', VERSION, '--commit', COMMIT, '--stage', 'wheel-current',
                         '--proofs', str(root / 'proofs')]
            with patch.object(check_wheel, 'ROOT', root), patch('sys.argv', arguments), \
                 patch.object(check_wheel, 'inspect_wheel'), \
                 patch.object(check_wheel.subprocess, 'run', side_effect=EnvironmentSelected) as run:
                with self.assertRaises(EnvironmentSelected):
                    check_wheel.main()
            prefix = Path(run.call_args.args[0][-1])
            self.assertFalse(prefix.is_relative_to(root / 'target'))
            self.assertEqual((cached / 'old-install').read_text(encoding='utf-8'), 'previous revision')


class WheelTestIsolationTests(unittest.TestCase):
    def test_isolated_helper_tests_get_support_but_not_the_source_package(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            check_wheel.copy_helper_tests(root)
            self.assertFalse((root / 'fetchloom').exists())
            self.assertTrue((root / 'test_helper.py').is_file())
            program = (
                'import sys; from pathlib import Path; sys.path.insert(0, sys.argv[1]); '
                'import wheel_scripts; '
                'assert Path(wheel_scripts.__file__).resolve().parent == Path(sys.argv[1]).resolve()'
            )
            result = subprocess.run([sys.executable, '-I', '-c', program, directory], cwd=directory,
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
