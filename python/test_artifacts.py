"""the release and artifact checks, against packages, proofs and stand-ins made here: they refuse
missing targets, unexecuted checks and wrong installed bytes. what only a real package, installer
or public site reaches runs in artifacts.yml and live.yml."""

import argparse
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import socket
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import urllib.error
import urllib.request
import warnings
import zipfile

import check_archive
from check_archive import SHIPPED, check_machine, unpack
import check_artifacts
from check_artifacts import TARGETS, archive_name, inventory, record, stages
import check_installer
import check_rar
from check_rar import extraction
import check_wheel
from check_wheel import inspect_wheel
import ci_journey
from ci_journey import DATA, DIGEST, Journey, Server, check_version, environment, fixtures, remove_tree, wheel_version
import release_sbom

VERSION = '0.1.0'
COMMIT = 'a' * 40
# the machine fields of each executable format, as the ELF, PE and Mach-O specifications number them
EM_X86_64, EM_AARCH64 = 62, 183
PE_AMD64, PE_ARM64 = 0x8664, 0xAA64
CPU_X86_64, CPU_ARM64 = 0x01000007, 0x0100000C


def elf(machine, ident=b'\x02\x01'):
    """the head of an ELF executable for ``machine``, ``ident`` naming its class and byte order."""
    body = bytearray(64)
    body[:6] = b'\x7fELF' + ident
    body[18:20] = machine.to_bytes(2, 'little')
    return bytes(body)


def pe(machine, signature=b'PE\0\0', at=0x80):
    """the head of a windows executable for ``machine``, its PE header ``at`` the offset its DOS
    header names."""
    body = bytearray(at + 8)
    body[:2] = b'MZ'
    body[60:64] = at.to_bytes(4, 'little')
    body[at:at + 4] = signature
    body[at + 4:at + 6] = machine.to_bytes(2, 'little')
    return bytes(body)


def macho(cpu, magic=b'\xcf\xfa\xed\xfe'):
    """the head of a mach-o executable for ``cpu``."""
    return magic + cpu.to_bytes(4, 'little') + bytes(24)


def wheel_files(binary, recorded=VERSION, notices=True):
    """the files of a wheel as maturin writes it, holding ``binary``, and the third-party
    licenses when ``notices``."""
    info = f'fetchloom-{recorded}.dist-info'
    metadata = f'Name: fetchloom\nVersion: {recorded}\nRequires-Python: >=3.10\n'
    files = {'fetchloom/__init__.py': b'', 'fetchloom/py.typed': b'',
             f'fetchloom-{recorded}.data/scripts/fetchloom': binary}
    if notices:
        metadata += 'License-File: THIRD-PARTY-LICENSES\n'
        files[f'{info}/licenses/THIRD-PARTY-LICENSES'] = b'crate notices\n'
    files[f'{info}/METADATA'] = metadata.encode()
    return files


def write_wheel(path, binary, recorded=VERSION, notices=True):
    write_zip(path, wheel_files(binary, recorded, notices).items())


def write_zip(path, entries):
    """a zip at ``path`` of ``entries``, each a name and its bytes, a name ending in / a folder."""
    with zipfile.ZipFile(path, 'w') as archive, warnings.catch_warnings():
        # a test may put one name in twice
        warnings.simplefilter('ignore', UserWarning)
        for name, body in entries:
            archive.writestr(name, body)


def write_tar(path, entries, program=0o755):
    """a tar.gz at ``path`` of ``entries``, each a name and its bytes, a name ending in / a folder;
    the program gets the mode ``program``."""
    with tarfile.open(path, 'w:gz') as archive:
        for name, body in entries:
            entry = tarfile.TarInfo(name.rstrip('/'))
            if name.endswith('/'):
                entry.type, entry.mode = tarfile.DIRTYPE, 0o755
                archive.addfile(entry)
            else:
                entry.size, entry.mode = len(body), program if name.endswith('/fetchloom') else 0o644
                archive.addfile(entry, io.BytesIO(body))


def release(target, program=None):
    """the folder of ``target``'s release archive and its entries, each shipped file holding its
    own name."""
    folder = f'fetchloom-{VERSION}-{target}'
    name = 'fetchloom.exe' if 'windows' in target else 'fetchloom'
    body = program if program is not None else pe(PE_AMD64) if 'windows' in target else elf(EM_X86_64)
    return folder, [(f'{folder}/{name}', body), *((f'{folder}/{shipped}', shipped.encode()) for shipped in SHIPPED)]


def wheel_name(target, version=VERSION):
    return f'fetchloom-{version}-py3-none-' + '.'.join(sorted(TARGETS[target]['wheel'])) + '.whl'


def proof(target, stage, version=VERSION):
    """what a passing ``stage`` of ``target``'s job records."""
    current = '3.14.7' if target == 'x86_64-apple-darwin' else '3.15.2'
    return {'ok': True, 'version': version, 'commit': COMMIT, 'binary_sha256': 'b' * 64,
            'helper': stage.startswith('wheel-'), 'python': '3.10.11' if stage == 'wheel-minimum' else current}


def packages(target, version=VERSION, wheel_version=VERSION):
    """the names of the packages ``target``'s job builds."""
    names = [archive_name(target, version), wheel_name(target, wheel_version)]
    if 'deb' in TARGETS[target]:
        names.append(f"fetchloom_{version}_{TARGETS[target]['deb']}.deb")
    return names


class InventoryTests(unittest.TestCase):
    TARGET = 'x86_64-unknown-linux-musl'

    def make(self, root, version=VERSION, wheel_version=VERSION):
        dist, proofs = root / 'dist', root / 'proofs'
        dist.mkdir()
        proofs.mkdir()
        for target in TARGETS:
            files = {}
            for name in packages(target, version, wheel_version):
                data = name.encode()
                (dist / name).write_bytes(data)
                files[name] = hashlib.sha256(data).hexdigest()
            folder = proofs / target
            folder.mkdir()
            (folder / 'verified.json').write_text(json.dumps({'target': target, 'version': version, 'commit': COMMIT,
                 'checks': {stage: proof(target, stage, version) for stage in stages(target)}, 'files': files}), encoding='utf-8')
        (proofs / 'source-package.json').write_text(json.dumps({'ok': True, 'version': version, 'commit': COMMIT}), encoding='utf-8')
        return dist, proofs

    def accepted(self, dist, proofs, version=VERSION):
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed):
            assets = inventory(dist, proofs, version, COMMIT)
        self.assertEqual(printed.getvalue(), f'verified all 6 targets and 14 assets for {COMMIT}\n')
        self.assertEqual(assets, {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in dist.iterdir()})

    def test_exact_six_target_set_is_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            self.accepted(*self.make(Path(directory)))

    def test_a_pre_release_wheel_is_named_as_python_spells_its_version(self):
        with tempfile.TemporaryDirectory() as directory:
            self.accepted(*self.make(Path(directory), '0.1.0-rc.1', '0.1.0rc1'), version='0.1.0-rc.1')

    def test_each_missing_or_mismatched_proof_or_asset_is_refused(self):
        archive, wheel, deb = packages(self.TARGET)

        def add(dist, files, name, data=b'extra'):
            (dist / name).write_bytes(data)
            files[name] = hashlib.sha256(data).hexdigest()

        def symlink(dist, files):
            outside = dist.parent / 'outside.tar.gz'
            outside.write_bytes((dist / archive).read_bytes())
            (dist / archive).unlink()
            (dist / archive).symlink_to(outside)

        def checks(document):
            return document['checks']

        cases = [
            ('target', self.TARGET, lambda dist, proofs, d: (proofs / self.TARGET / 'verified.json').unlink(),
             'target inventory differs'),
            ('target twice', self.TARGET, lambda dist, proofs, d: shutil.copytree(proofs / self.TARGET, proofs / 'again'),
             'target inventory differs'),
            ('revision', self.TARGET, lambda dist, proofs, d: d.update(commit='c' * 40), f'wrong revision in {self.TARGET}'),
            ('check', self.TARGET, lambda dist, proofs, d: checks(d).pop('archive'), f'missing or unexpected check in {self.TARGET}'),
            ('extra check', self.TARGET, lambda dist, proofs, d: checks(d).update({'installer-pwsh': checks(d)['archive']}),
             f'missing or unexpected check in {self.TARGET}'),
            ('failed', self.TARGET, lambda dist, proofs, d: checks(d)['deb'].update(ok=False), f'{self.TARGET}: deb did not pass'),
            ('stage revision', self.TARGET, lambda dist, proofs, d: checks(d)['deb'].update(version='0.0.9'),
             f'{self.TARGET}: deb used another revision'),
            ('hash', self.TARGET, lambda dist, proofs, d: checks(d)['deb'].update(binary_sha256='B' * 64), 'invalid executable hash'),
            ('executables', self.TARGET, lambda dist, proofs, d: checks(d)['deb'].update(binary_sha256='c' * 64),
             f'{self.TARGET}: checks did not use the same executable'),
            ('helper', self.TARGET, lambda dist, proofs, d: checks(d)['wheel-current'].update(helper=False),
             f'{self.TARGET}: the installed helper was not exercised'),
            ('python', self.TARGET, lambda dist, proofs, d: checks(d)['wheel-current'].update(python='3.13.9'),
             rf'{self.TARGET}: the wheel was not exercised on Python 3\.15\.x'),
            ('intel mac python', 'x86_64-apple-darwin', lambda dist, proofs, d: checks(d)['wheel-current'].update(python='3.15.2'),
             r'x86_64-apple-darwin: the wheel was not exercised on Python 3\.14\.x'),
            ('minimum python', self.TARGET, lambda dist, proofs, d: checks(d)['wheel-minimum'].update(python='3.11.9'),
             'Python 3.10 wheel was not exercised'),
            ('two wheels', self.TARGET, lambda dist, proofs, d: add(dist, d['files'], wheel.replace('.whl', '.extra.whl')),
             f'{self.TARGET}: expected exactly one wheel'),
            ('wheel tags', self.TARGET, lambda dist, proofs, d: (d['files'].pop(wheel),
                                                                 add(dist, d['files'], f'fetchloom-{VERSION}-py3-none-manylinux2014_x86_64.whl')),
             'wrong wheel target'),
            ('extra package', self.TARGET, lambda dist, proofs, d: add(dist, d['files'], 'notes.txt'),
             f'{self.TARGET}: unexpected package files'),
            ('asset', self.TARGET, lambda dist, proofs, d: (dist / deb).unlink(), f'missing or changed asset: {deb}'),
            ('checksum', self.TARGET, lambda dist, proofs, d: (dist / archive).write_bytes(b'wrong'), f'missing or changed asset: {archive}'),
            ('unlisted', self.TARGET, lambda dist, proofs, d: (dist / 'notes.txt').write_bytes(b''), 'unlisted or missing release assets'),
            ('source', self.TARGET, lambda dist, proofs, d: (proofs / 'source-package.json').unlink(), 'missing source-package install proof'),
            ('source failed', self.TARGET, lambda dist, proofs, d: (proofs / 'source-package.json').write_text(
                json.dumps({'ok': False, 'version': VERSION, 'commit': COMMIT})), 'source package did not pass'),
            ('source revision', self.TARGET, lambda dist, proofs, d: (proofs / 'source-package.json').write_text(
                json.dumps({'ok': True, 'version': VERSION, 'commit': 'c' * 40})), 'source package did not pass'),
        ]
        if os.name != 'nt':
            cases.append(('link', self.TARGET, lambda dist, proofs, d: symlink(dist, d['files']), f'missing or changed asset: {archive}'))
        for case, target, change, said in cases:
            with self.subTest(case), tempfile.TemporaryDirectory() as directory:
                dist, proofs = self.make(Path(directory))
                path = proofs / target / 'verified.json'
                document = json.loads(path.read_text(encoding='utf-8'))
                change(dist, proofs, document)
                if path.exists():
                    path.write_text(json.dumps(document), encoding='utf-8')
                with self.assertRaisesRegex(AssertionError, said), contextlib.redirect_stdout(io.StringIO()):
                    inventory(dist, proofs, VERSION, COMMIT)

    def test_two_targets_may_not_claim_one_package(self):
        # as if both windows targets built the same x86_64 wheel
        shared = dict(TARGETS, **{'aarch64-pc-windows-msvc': {'wheel': {'win_amd64'}}})
        with tempfile.TemporaryDirectory() as directory, patch.dict(check_artifacts.TARGETS, shared):
            dist, proofs = self.make(Path(directory))
            with self.assertRaisesRegex(AssertionError, f'duplicate asset {wheel_name("x86_64-pc-windows-msvc")}'):
                inventory(dist, proofs, VERSION, COMMIT)


class StageTests(unittest.TestCase):
    def test_each_target_records_the_checks_artifacts_yml_runs_for_it(self):
        # read from the conditions of artifacts.yml's steps; every target also checks its archive and
        # its wheel on the newest python its runner has
        expected = {
            'x86_64-unknown-linux-musl': {'wheel-minimum', 'installer-sh', 'rar', 'deb'},
            'aarch64-unknown-linux-musl': {'installer-sh', 'deb'},
            'x86_64-pc-windows-msvc': {'wheel-minimum', 'installer-powershell', 'installer-pwsh', 'rar'},
            'aarch64-pc-windows-msvc': set(),
            'x86_64-apple-darwin': {'installer-sh'},
            'aarch64-apple-darwin': {'installer-sh', 'rar'},
        }
        self.assertEqual(set(expected), set(TARGETS))
        for target, extra in expected.items():
            with self.subTest(target):
                self.assertEqual(stages(target), {'archive', 'wheel-current', *extra})


class RecordTests(unittest.TestCase):
    """check_artifacts.py record as each target's job runs it, then inventory as the release does."""

    def build(self, root, target, missing=()):
        dist, proofs = root / 'jobs' / target, root / 'proofs' / target
        dist.mkdir(parents=True)
        proofs.mkdir(parents=True)
        for name in packages(target):
            (dist / name).write_bytes(name.encode())
        for stage in stages(target) - set(missing):
            (proofs / f'{stage}.json').write_text(json.dumps(proof(target, stage)), encoding='utf-8')
        return dist, proofs

    def command(self, *arguments):
        printed = io.StringIO()
        with patch.object(sys, 'argv', ['check_artifacts.py', *map(str, arguments), '--version', VERSION, '--commit', COMMIT]), \
             contextlib.redirect_stdout(printed):
            check_artifacts.main()
        return printed.getvalue()

    def test_the_records_of_every_target_make_a_full_inventory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            published = root / 'release'
            published.mkdir()
            for target in TARGETS:
                dist, proofs = self.build(root, target)
                self.assertEqual(self.command('record', dist, proofs, '--target', target), '')
                document = json.loads((proofs / 'verified.json').read_text(encoding='utf-8'))
                self.assertEqual(document, {'target': target, 'version': VERSION, 'commit': COMMIT,
                                            'checks': {stage: proof(target, stage) for stage in stages(target)},
                                            'files': {name: hashlib.sha256(name.encode()).hexdigest() for name in packages(target)}})
                for path in dist.iterdir():
                    shutil.copyfile(path, published / path.name)
            (root / 'proofs/source-package.json').write_text(json.dumps({'ok': True, 'version': VERSION, 'commit': COMMIT}),
                                                            encoding='utf-8')
            self.assertEqual(self.command('inventory', published, root / 'proofs'),
                             f'verified all 6 targets and 14 assets for {COMMIT}\n')

    def test_a_check_that_left_no_proof_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            dist, proofs = self.build(Path(directory), 'aarch64-pc-windows-msvc', missing=['wheel-current'])
            with self.assertRaisesRegex(AssertionError, 'missing executed check: aarch64-pc-windows-msvc/wheel-current'):
                record(dist, proofs, 'aarch64-pc-windows-msvc', VERSION, COMMIT)
            self.assertFalse((proofs / 'verified.json').exists())

    def test_record_needs_a_target(self):
        with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(AssertionError, 'record requires a target'):
            self.command('record', directory, directory)


class PackageContentTests(unittest.TestCase):
    def test_tar_links_and_extra_entries_are_refused(self):
        for link in [True, False]:
            with self.subTest(link=link), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                path = root / 'bad.tar.gz'
                with tarfile.open(path, 'w:gz') as archive:
                    member = tarfile.TarInfo('../outside' if not link else 'fetchloom-0.1.0-x86_64-unknown-linux-musl/fetchloom')
                    if link:
                        member.type, member.mode = tarfile.SYMTYPE, 0o755
                        member.linkname = '../outside'
                    archive.addfile(member)
                with self.assertRaisesRegex(AssertionError, 'unexpected or duplicate archive entry'):
                    unpack(path, 'x86_64-unknown-linux-musl', VERSION, root / 'unpacked')
                self.assertFalse((root / 'outside').exists())

    def test_an_archive_carries_the_notices_and_the_manual_page(self):
        target = 'x86_64-unknown-linux-musl'
        folder, entries = release(target)
        for missing in [None, 'THIRD-PARTY-LICENSES', 'fetchloom.1']:
            with self.subTest(missing=missing), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                path = root / 'release.tar.gz'
                write_tar(path, [(f'{folder}/', b''), *(entry for entry in entries if entry[0] != f'{folder}/{missing}')])
                if missing is None:
                    binary = unpack(path, target, VERSION, root / 'unpacked')
                    self.assertEqual((root / 'unpacked/fetchloom.1').read_bytes(), b'fetchloom.1')
                    self.assertEqual((binary, binary.read_bytes()), (root / 'unpacked/fetchloom', elf(EM_X86_64)))
                    self.assertTrue(os.access(binary, os.X_OK))
                else:
                    with self.assertRaisesRegex(AssertionError, 'release archive is incomplete'):
                        unpack(path, target, VERSION, root / 'unpacked')

    def test_a_windows_zip_unpacks_its_program_and_notices(self):
        target = 'aarch64-pc-windows-msvc'
        folder, entries = release(target, pe(PE_ARM64))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_zip(root / 'release.zip', [(f'{folder}/', b''), *entries])
            binary = unpack(root / 'release.zip', target, VERSION, root / 'unpacked')
            self.assertEqual((binary, binary.read_bytes()), (root / 'unpacked/fetchloom.exe', pe(PE_ARM64)))
            self.assertEqual(sorted(path.name for path in binary.parent.iterdir()), sorted(['fetchloom.exe', *SHIPPED]))

    def test_an_archive_holds_its_own_files_once_in_its_own_folder(self):
        for suffix, target, write in [('.tar.gz', 'x86_64-unknown-linux-musl', write_tar), ('.zip', 'x86_64-pc-windows-msvc', write_zip)]:
            folder, entries = release(target)
            for case, changed, said in [
                ('another folder', [('other/', b''), *entries], 'unexpected archive directory'),
                ('a file twice', [*entries, entries[1]], 'unexpected or duplicate archive entry'),
                ('another file', [*entries, (f'{folder}/notes.txt', b'')], 'unexpected or duplicate archive entry'),
                ('no notice', [entry for entry in entries if not entry[0].endswith('/NOTICE')], 'release archive is incomplete'),
            ]:
                with self.subTest(suffix=suffix, case=case), tempfile.TemporaryDirectory() as directory:
                    path = Path(directory) / f'release{suffix}'
                    write(path, changed)
                    with self.assertRaisesRegex(AssertionError, said):
                        unpack(path, target, VERSION, Path(directory) / 'unpacked')

    def test_a_tar_keeps_the_program_runnable_and_no_file_is_oversized(self):
        target = 'x86_64-unknown-linux-musl'
        folder, entries = release(target)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'release.tar.gz'
            write_tar(path, entries, program=0o644)
            with self.assertRaisesRegex(AssertionError, 'archive lost executable permissions'):
                unpack(path, target, VERSION, Path(directory) / 'unpacked')
            for suffix, write, target in [('.tar.gz', write_tar, target), ('.zip', write_zip, 'x86_64-pc-windows-msvc')]:
                with self.subTest(suffix=suffix):
                    folder, entries = release(target)
                    path = Path(directory) / f'large{suffix}'
                    write(path, entries)
                    # the program's 64 or 136 bytes are past this cap, as a 256 MiB file is past the real one
                    with patch.object(check_archive, 'LARGEST', 64), self.assertRaisesRegex(AssertionError, 'oversized release file'):
                        unpack(path, target, VERSION, Path(directory) / 'unpacked')

    def test_the_program_is_built_for_its_target(self):
        for target, body, said in [
            ('x86_64-unknown-linux-musl', elf(EM_X86_64), None),
            ('aarch64-unknown-linux-musl', elf(EM_AARCH64), None),
            ('aarch64-unknown-linux-musl', elf(EM_X86_64), 'wrong ELF architecture'),
            ('x86_64-unknown-linux-musl', elf(EM_X86_64, ident=b'\x01\x01'), 'not a 64-bit little-endian ELF'),
            ('x86_64-unknown-linux-musl', elf(EM_X86_64, ident=b'\x02\x02'), 'not a 64-bit little-endian ELF'),
            ('x86_64-pc-windows-msvc', pe(PE_AMD64), None),
            ('aarch64-pc-windows-msvc', pe(PE_ARM64, at=0xF8), None),
            ('x86_64-pc-windows-msvc', pe(PE_ARM64), 'wrong PE architecture'),
            ('x86_64-pc-windows-msvc', pe(PE_AMD64, signature=b'NE\0\0'), 'invalid PE signature'),
            ('x86_64-pc-windows-msvc', elf(EM_X86_64), 'not a Windows executable'),
            ('x86_64-apple-darwin', macho(CPU_X86_64), None),
            ('aarch64-apple-darwin', macho(CPU_ARM64), None),
            ('x86_64-apple-darwin', macho(CPU_ARM64), 'wrong Mach-O architecture'),
            ('aarch64-apple-darwin', macho(CPU_ARM64, magic=b'\xce\xfa\xed\xfe'), 'not a 64-bit Mach-O'),
        ]:
            with self.subTest(target=target, said=said), tempfile.TemporaryDirectory() as directory:
                binary = Path(directory) / 'fetchloom'
                binary.write_bytes(body)
                if said is None:
                    check_machine(binary, target)
                else:
                    with self.assertRaisesRegex(AssertionError, said):
                        check_machine(binary, target)


class ArchiveCheckTests(unittest.TestCase):
    """check_archive.py: the archive must ship the checkout's notices and the checked build, which
    the journey then runs."""

    TARGET = 'x86_64-unknown-linux-musl'

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name).resolve()
        self.checkout = self.root / 'checkout'
        for name, source in SHIPPED.items():
            (self.checkout / source).parent.mkdir(parents=True, exist_ok=True)
            (self.checkout / source).write_bytes(f'{name} as committed\n'.encode())
        self.built = self.root / 'built'
        self.built.write_bytes(elf(EM_X86_64))
        (self.root / 'dist').mkdir()

    def check(self, program=None, notice=None):
        folder = f'fetchloom-{VERSION}-{self.TARGET}'
        entries = [(f'{folder}/fetchloom', self.built.read_bytes() if program is None else program)]
        for name, source in SHIPPED.items():
            body = notice if name == 'NOTICE' and notice is not None else (self.checkout / source).read_bytes()
            entries.append((f'{folder}/{name}', body))
        write_tar(self.root / 'dist' / archive_name(self.TARGET, VERSION), entries)
        arguments = ['check_archive.py', '--target', self.TARGET, '--version', VERSION, '--commit', COMMIT,
                     '--built', str(self.built), '--proofs', str(self.root / 'proofs')]
        here = os.getcwd()
        # it unpacks into .release in the working folder, as in the checkout ci runs it from
        os.chdir(self.root)
        try:
            with patch.object(check_archive, 'ROOT', self.checkout), patch.object(sys, 'argv', arguments), \
                 patch('subprocess.run') as run:
                check_archive.main()
        finally:
            os.chdir(here)
        return run

    def test_the_checked_build_goes_on_to_the_journey(self):
        run = self.check()
        unpacked = self.root / '.release/artifact-bin' / self.TARGET / 'fetchloom'
        self.assertEqual(unpacked.read_bytes(), self.built.read_bytes())
        self.assertEqual(run.call_args.args[0], [sys.executable, 'python/ci_journey.py', '--binary', str(unpacked),
                                                 '--version', VERSION, '--commit', COMMIT,
                                                 '--report', str(self.root / 'proofs/archive.json')])
        self.assertEqual(run.call_args.kwargs, {'check': True})

    def test_a_changed_notice_another_build_or_machine_is_refused(self):
        for case, change, said in [('notice', {'notice': b'edited\n'}, 'archive changed NOTICE'),
                                   ('build', {'program': elf(EM_X86_64) + b'rebuilt'}, 'differs from the checked release build'),
                                   ('machine', {'program': elf(EM_AARCH64)}, 'wrong ELF architecture')]:
            with self.subTest(case), self.assertRaisesRegex(AssertionError, said):
                self.check(**change)


class WheelContentTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.binary = self.root / 'fetchloom'
        self.binary.write_bytes(b'checked binary')

    def test_wheel_must_bundle_the_tested_binary_and_typed_helper(self):
        for wrong in [False, True]:
            wheel = self.root / 'package.whl'
            write_wheel(wheel, b'other binary' if wrong else self.binary.read_bytes())
            if wrong:
                with self.assertRaisesRegex(AssertionError, 'wheel rebuilt or omitted the checked executable'):
                    inspect_wheel(wheel, VERSION, self.binary)
            else:
                inspect_wheel(wheel, VERSION, self.binary)

    def test_a_pre_release_wheel_records_python_spelling_of_its_version(self):
        wheel = self.root / 'package.whl'
        write_wheel(wheel, self.binary.read_bytes(), '0.1.0rc1')
        inspect_wheel(wheel, '0.1.0-rc.1', self.binary)

    def test_a_wheel_carries_its_identity_helper_licenses_and_one_executable(self):
        info = f'fetchloom-{VERSION}.dist-info'

        def without(name):
            return lambda files: files.pop(name)

        def metadata(old, new):
            return lambda files: files.update({f'{info}/METADATA': files[f'{info}/METADATA'].replace(old, new)})

        for case, change, said in [
            ('no metadata', without(f'{info}/METADATA'), 'wheel has no unique metadata'),
            ('two metadata', lambda files: files.update({'other-0.1.dist-info/METADATA': b''}), 'wheel has no unique metadata'),
            ('another name', metadata(b'Name: fetchloom', b'Name: fetchloom-cli'), 'wrong wheel identity'),
            ('another version', metadata(b'Version: 0.1.0', b'Version: 0.1.1'), 'wrong wheel identity'),
            ('another floor', metadata(b'>=3.10', b'>=3.9'), 'wheel changed the Python floor'),
            ('no helper', without('fetchloom/__init__.py'), 'wheel omitted its helper'),
            ('no typed marker', without('fetchloom/py.typed'), 'wheel omitted its helper'),
            ('notices not named', metadata(b'License-File: THIRD-PARTY-LICENSES\n', b''), 'wheel omitted the third-party licenses'),
            ('notices left out', without(f'{info}/licenses/THIRD-PARTY-LICENSES'), 'wheel omitted the third-party licenses'),
            ('two executables', lambda files: files.update({'extra.data/scripts/fetchloom': b'checked binary'}),
             'wheel has no unique executable'),
        ]:
            with self.subTest(case):
                files = wheel_files(self.binary.read_bytes())
                change(files)
                wheel = self.root / f'{case}.whl'
                write_zip(wheel, files.items())
                with self.assertRaisesRegex(AssertionError, said):
                    inspect_wheel(wheel, VERSION, self.binary)


class ReleaseSbomTests(unittest.TestCase):
    OURS = json.dumps({'bomFormat': 'CycloneDX', 'metadata': {'component': {'name': 'fetchloom', 'version': VERSION}}})

    def wheel(self, folder, target, body, name=None):
        sboms = body if isinstance(body, dict) else {f'fetchloom-{VERSION}.dist-info/sboms/fetchloom.cyclonedx.json': body}
        write_zip(folder / (name or wheel_name(target)), sboms.items())

    def test_the_release_sbom_is_the_one_in_the_x86_64_linux_wheel(self):
        for body, kept in [(self.OURS, True), (self.OURS.replace(VERSION, '0.0.9'), False), ('{"bomFormat": "SPDX"}', False)]:
            with self.subTest(body=body), tempfile.TemporaryDirectory() as directory:
                dist, out = Path(directory) / 'dist', Path(directory) / 'sbom'
                dist.mkdir()
                self.wheel(dist, 'x86_64-unknown-linux-musl', body)
                self.wheel(dist, 'aarch64-apple-darwin', '{}')
                if kept:
                    self.assertEqual(release_sbom.release_sbom(dist, out, VERSION).read_text(), self.OURS)
                    self.assertEqual([path.name for path in out.iterdir()], [f'fetchloom-{VERSION}.cdx.json'])
                else:
                    with self.assertRaisesRegex(AssertionError, f'is not one of fetchloom {VERSION}'):
                        release_sbom.release_sbom(dist, out, VERSION)

    def test_one_x86_64_linux_wheel_with_one_sbom_is_needed(self):
        target = 'x86_64-unknown-linux-musl'
        reordered = f'fetchloom-{VERSION}-py3-none-' + '.'.join(sorted(TARGETS[target]['wheel'], reverse=True)) + '.whl'
        sbom = f'fetchloom-{VERSION}.dist-info/sboms/fetchloom.cyclonedx.json'
        for case, wheels, said in [
            ('no wheel', [], 'expected one x86_64 linux wheel'),
            ('two wheels', [(self.OURS, None), (self.OURS, reordered)], 'expected one x86_64 linux wheel'),
            ('no sbom', [({'fetchloom/__init__.py': b''}, None)], 'holds no single sbom'),
            ('two sboms', [({sbom: self.OURS, 'other.dist-info/sboms/fetchloom.cyclonedx.json': self.OURS}, None)],
             'holds no single sbom'),
        ]:
            with self.subTest(case), tempfile.TemporaryDirectory() as directory:
                dist = Path(directory)
                for body, name in wheels:
                    self.wheel(dist, target, body, name)
                with self.assertRaisesRegex(AssertionError, said):
                    release_sbom.release_sbom(dist, dist / 'sbom', VERSION)

    def test_the_command_says_where_it_wrote_the_sbom(self):
        with tempfile.TemporaryDirectory() as directory:
            dist, out = Path(directory) / 'dist', Path(directory) / 'sbom'
            dist.mkdir()
            self.wheel(dist, 'x86_64-unknown-linux-musl', self.OURS)
            printed = io.StringIO()
            with patch.object(sys, 'argv', ['release_sbom.py', str(dist), str(out), '--version', VERSION]), \
                 contextlib.redirect_stdout(printed):
                release_sbom.main()
        self.assertEqual(printed.getvalue(), f'wrote {out / f"fetchloom-{VERSION}.cdx.json"}\n')


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

    def test_a_build_must_name_the_commit_it_came_from(self):
        said = f'fetchloom 0.1.0\ncommit {COMMIT}\n'
        with patch('ci_journey.subprocess.check_output', return_value=said) as run:
            self.assertEqual(check_version(Path('fetchloom'), VERSION, commit=COMMIT), said)
            self.assertEqual(run.call_args.args[0], ['fetchloom', '--version', '-v'])
            with self.assertRaisesRegex(AssertionError, f'the artifact does not identify commit {"b" * 40}'):
                check_version(Path('fetchloom'), VERSION, commit='b' * 40)


class LoopbackServerTests(unittest.TestCase):
    def test_serving_needs_no_name_lookup(self):
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with patch('socket.getfqdn', side_effect=AssertionError('the server looked up a name')), \
             Server({'/file': b'body'}) as server, opener.open(server.url('/file'), timeout=10) as response:
            self.assertEqual(response.read(), b'body')

    def test_the_server_answers_each_name_it_holds_and_counts_every_request(self):
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        body = b'sepal_length\n'
        with Server({"/my data's.csv": body}) as server:
            url = server.url("/my data's.csv")
            self.assertEqual(url, server.origin + '/my%20data%27s.csv')
            with opener.open(urllib.request.Request(url, method='HEAD'), timeout=10) as response:
                self.assertEqual((response.status, response.read()), (200, b''))
                self.assertEqual((response.headers['Content-Length'], response.headers['ETag']),
                                 (str(len(body)), '"' + hashlib.sha256(body).hexdigest() + '"'))
            # a body after HEAD's headers would be the next answer's start, which urllib never reads
            port = int(server.origin.rsplit(':', 1)[1])
            with socket.create_connection(('127.0.0.1', port), timeout=10) as connection:
                connection.sendall(b"HEAD /my%20data%27s.csv HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
                answer = b''.join(iter(lambda: connection.recv(65536), b''))
            self.assertEqual(answer.split(b'\r\n\r\n', 1)[1], b'')
            with opener.open(url, timeout=10) as response:
                self.assertEqual(response.read(), body)
            with self.assertRaises(urllib.error.HTTPError) as missing:
                opener.open(server.url('/other.csv'), timeout=10)
            missing.exception.close()
            self.assertEqual(missing.exception.code, 404)
            self.assertEqual(server.count(), 4)
            self.assertEqual(server.requests, [('HEAD', "/my data's.csv"), ('HEAD', "/my data's.csv"), ('GET', "/my data's.csv"),
                                               ('GET', '/other.csv')])


class JourneyTests(unittest.TestCase):
    def test_the_journey_runs_with_no_credentials_proxies_or_settings_of_the_machine(self):
        machine = {'PATH': os.environ['PATH'], 'LANG': 'en_US.UTF-8', 'FETCHLOOM_CACHE': 'mine', 'FETCHLOOM_OFFLINE': '1',
                   'AWS_SECRET_ACCESS_KEY': 's', 'HF_TOKEN': 't', 'KAGGLE_KEY': 'k', 'DATAVERSE_KEY': 'd',
                   'ONEDRIVECOMMERCIAL': 'od', 'GITHUB_TOKEN': 'g', 'https_proxy': 'http://proxy:3128', 'NO_PROXY': 'corp',
                   'PYTHONPATH': 'src', 'PYTHONHOME': 'py', 'XDG_CACHE_HOME': 'xc', 'XDG_CONFIG_HOME': 'xg', 'NETRC': 'n'}
        home, cache = Path('journey', 'home'), Path('journey', 'cache')
        given = {'PATH': os.environ['PATH'], 'LANG': 'en_US.UTF-8', 'HOME': str(home), 'USERPROFILE': str(home),
                 'APPDATA': str(home), 'LOCALAPPDATA': str(home), 'FETCHLOOM_KEYRING': '0', 'CI': '1', 'CLICOLOR': '0',
                 'NO_PROXY': '127.0.0.1', 'LC_ALL': 'C.UTF-8'}
        with patch.dict(os.environ, machine, clear=True):
            self.assertEqual(environment(home), given)
            self.assertEqual(environment(home, cache, 'http://127.0.0.1:9'),
                             {**given, 'FETCHLOOM_CACHE': str(cache), 'FETCHLOOM_ALLOWED_ORIGINS': 'http://127.0.0.1:9'})

    def test_a_clone_takes_the_manifest_and_the_pins(self):
        with tempfile.TemporaryDirectory() as directory, Server({}) as server:
            journey = Journey(Path(directory, 'fetchloom'), Path(directory), server)
            source = journey.project('original')
            for name in ['data.toml', 'data.lock']:
                (source / name).write_text(name, encoding='utf-8')
            (source / 'data').mkdir()
            self.assertEqual(sorted(path.name for path in journey.clone(source, 'first').iterdir()), ['data.lock', 'data.toml'])
            (source / 'data.lock.d').mkdir()
            (source / 'data.lock.d/clean.lock').write_text('recipe pins', encoding='utf-8')
            second = journey.clone(source, 'second')
            self.assertEqual(sorted(path.name for path in second.iterdir()), ['data.lock', 'data.lock.d', 'data.toml'])
            self.assertEqual((second / 'data.lock.d/clean.lock').read_text(encoding='utf-8'), 'recipe pins')

    def test_a_tree_of_placed_read_only_files_is_removed(self):
        with tempfile.TemporaryDirectory() as directory:
            data = Path(directory, 'data')
            (data / 'iris').mkdir(parents=True)
            (data / 'iris/iris.csv').write_bytes(DATA)
            # as a placed link to the cache's copy is, which windows refuses to delete
            (data / 'iris/iris.csv').chmod(stat.S_IREAD)
            remove_tree(data)
            self.assertFalse(data.exists())
            remove_tree(data)
        # windows refuses to delete it, so it is made writable and deleted again
        with tempfile.TemporaryDirectory() as directory:
            placed = Path(directory, 'iris.csv')
            placed.write_bytes(DATA)
            placed.chmod(stat.S_IREAD)
            retried = []

            def unlink(name):
                retried.append(stat.S_IMODE(os.stat(name).st_mode) & stat.S_IWRITE)
                os.unlink(name)

            def refused(path, onerror):
                onerror(unlink, str(placed), (PermissionError, PermissionError(13, 'Access is denied'), None))

            with patch.object(ci_journey.shutil, 'rmtree', refused):
                remove_tree(Path(directory))
            self.assertEqual((retried, placed.exists()), ([stat.S_IWRITE], False))

    def test_the_fixtures_hold_the_bytes_the_journey_checks(self):
        served = fixtures()
        self.assertEqual((served['/iris.csv'], hashlib.sha256(DATA).hexdigest()), (DATA, DIGEST))
        with zipfile.ZipFile(io.BytesIO(served['/bundle.zip'])) as bundle:
            self.assertEqual({name: bundle.read(name) for name in bundle.namelist()},
                             {'bundle/train.txt': b'archive fixture\n', 'bundle/empty': b''})

    def test_the_journey_reports_what_it_checked(self):
        started = []

        class Checked:
            def __init__(self, binary, root, server):
                started.append((binary, root, server.origin))

            def check(self, version, commit, helper):
                return {'ok': True, 'version': version, 'commit': commit, 'helper': helper}

        with tempfile.TemporaryDirectory() as directory:
            work, report = Path(directory, 'work'), Path(directory, 'proofs/x86_64/archive.json')
            work.mkdir()
            arguments = ['ci_journey.py', '--binary', 'fetchloom', '--version', '0.1.0-rc.1', '--commit', COMMIT,
                         '--work-root', str(work), '--report', str(report)]
            printed = io.StringIO()
            with patch.object(ci_journey, 'Journey', Checked), patch.object(sys, 'argv', arguments), \
                 contextlib.redirect_stdout(printed):
                ci_journey.main()
            result = {'ok': True, 'version': '0.1.0-rc.1', 'commit': COMMIT, 'helper': False}
            self.assertEqual(report.read_text(encoding='utf-8'), json.dumps(result, indent=2) + '\n')
            self.assertEqual(printed.getvalue(), json.dumps(result) + '\n')
            (binary, root, origin), = started
            self.assertEqual((binary, root.parent), (Path('fetchloom'), work))
            self.assertTrue(origin.startswith('http://127.0.0.1:'))
            self.assertEqual(list(work.iterdir()), [])
            # without --report, as a local run gives it, the proof goes to stdout alone
            printed = io.StringIO()
            with patch.object(ci_journey, 'Journey', Checked), patch.object(sys, 'argv', arguments[:-2]), \
                 contextlib.redirect_stdout(printed):
                ci_journey.main()
            self.assertEqual(printed.getvalue(), json.dumps(result) + '\n')
            self.assertEqual(sorted(path.name for path in Path(directory).iterdir()), ['proofs', 'work'])
        with patch.object(ci_journey, 'helper_worker') as worker, patch.object(ci_journey, 'Journey') as journey, \
             patch.object(sys, 'argv', ['ci_journey.py', '--helper-worker', 'project', '--binary', 'fetchloom']):
            ci_journey.main()
        worker.assert_called_once_with(Path('project'), Path('fetchloom'))
        journey.assert_not_called()


@unittest.skipIf(os.name == 'nt', 'stands in for fetchloom with a python script')
class JourneyRunTests(unittest.TestCase):
    # stands in for fetchloom: it answers with the status and exits with the code its last two
    # arguments name
    PROGRAM = ("import json, os, sys\n"
               "status, code = sys.argv[-2:]\n"
               "print(json.dumps({'status': status, 'arguments': sys.argv[1:], 'cache': os.environ['FETCHLOOM_CACHE']}))\n"
               "sys.exit(int(code))\n")

    def test_a_run_must_end_as_it_was_meant_to(self):
        with tempfile.TemporaryDirectory() as directory, Server({}) as server:
            root = Path(directory)
            program = root / 'fetchloom'
            program.write_text(f'#!{sys.executable}\n{self.PROGRAM}', encoding='utf-8')
            program.chmod(0o755)
            journey = Journey(program, root, server)
            project = journey.project('project')
            printed = io.StringIO()
            with contextlib.redirect_stdout(printed):
                answer = journey.run(project, 'ok', '0')
                self.assertEqual((answer['arguments'], answer['cache']), (['--json', '-C', str(project), 'ok', '0'], str(root / 'cache')))
                self.assertEqual(journey.run(project, 'error', '2', success=False)['status'], 'error')
                self.assertEqual(journey.run(project, 'ok', '0', cache=root / 'fresh')['cache'], str(root / 'fresh'))
                for status, code, success in [('error', '0', True), ('ok', '2', True), ('ok', '0', False), ('error', '0', False)]:
                    with self.subTest(status=status, code=code, success=success), self.assertRaises(AssertionError):
                        journey.run(project, status, code, success=success)
        self.assertEqual(printed.getvalue().splitlines()[:2], ['+ fetchloom ok 0 => 0', '+ fetchloom error 2 => 2'])


class RarTests(unittest.TestCase):
    # what fetchloom says on each system of data/sample/sample.rar, as src/archive/tests.rs has it
    HELP = {
        'linux': 'unpack it with unrar from your package manager: `unrar x data/sample/sample.rar sample-unpacked/`. '
                 'extract = false stops this warning',
        'darwin': 'unpack it with unar: `brew install unar`, then `unar -D -o sample-unpacked data/sample/sample.rar`. '
                  'extract = false stops this warning',
        'win32': r'unpack it with 7-Zip: `winget install 7zip.7zip`, then `& "C:\Program Files\7-Zip\7z.exe" x '
                 r'data\sample\sample.rar -osample-unpacked`. extract = false stops this warning',
    }

    def warnings(self, help):
        return [{'kind': 'archive.packed', 'message': 'sample.rar is a rar archive', 'details': [], 'help': help}]

    def test_the_command_for_this_system_is_the_one_run(self):
        for system, command in [('linux', 'unrar x data/sample/sample.rar sample-unpacked/'),
                                ('darwin', 'unar -D -o sample-unpacked data/sample/sample.rar'),
                                ('win32', r'& "C:\Program Files\7-Zip\7z.exe" x data\sample\sample.rar -osample-unpacked')]:
            with self.subTest(system):
                self.assertEqual(extraction(self.warnings(self.HELP[system]), system), command)
                other = self.HELP[system] + r', or `& "C:\Program Files\WinRAR\UnRAR.exe" x sample.rar`'
                self.assertEqual(extraction(self.warnings(other), system), command)
        quoted = ("unpack it with unrar from your package manager: `unrar x '../data/my data/it'\\''s.RAR' "
                  "'../it'\\''s-unpacked/'`. --no-extract stops this warning")
        self.assertEqual(extraction(self.warnings(quoted), 'linux'), "unrar x '../data/my data/it'\\''s.RAR' '../it'\\''s-unpacked/'")

    def test_the_proof_records_each_case_the_check_ran(self):
        with tempfile.TemporaryDirectory() as directory:
            proofs = Path(directory, 'proofs/x86_64-unknown-linux-musl')
            result = {'ok': True, 'version': VERSION, 'commit': COMMIT, 'binary_sha256': 'b' * 64, 'cases': []}
            arguments = ['check_rar.py', '--binary', 'fetchloom', '--version', VERSION, '--commit', COMMIT, '--proofs', str(proofs)]
            with patch.object(check_rar, 'check', return_value=result) as check, patch.object(sys, 'argv', arguments):
                check_rar.main()
            check.assert_called_once_with(Path('fetchloom').resolve(), VERSION, COMMIT)
            self.assertEqual((proofs / 'rar.json').read_text(encoding='utf-8'), json.dumps(result, indent=2) + '\n')

    def test_anything_but_one_rar_warning_with_one_command_is_refused(self):
        packed, = self.warnings(self.HELP['linux'])
        for case, given, system, said in [
            ('no warning', [], 'linux', 'expected one rar warning'),
            ('two warnings', [packed, packed], 'linux', 'expected one rar warning'),
            ('another kind', [dict(packed, kind='archive.unknown')], 'linux', 'expected one rar warning'),
            ('another system', [packed], 'darwin', 'no unique extraction command'),
            ('two commands', [dict(packed, help=packed['help'] + ', or `unrar e x.rar`')], 'linux', 'no unique extraction command'),
        ]:
            with self.subTest(case), self.assertRaisesRegex(AssertionError, said):
                extraction(given, system)


class InstallerCheckTests(unittest.TestCase):
    """check_installer.py against a stand-in release: the installer is real, the journey after it a
    stub, since it needs a real fetchloom."""

    def release(self, root, target):
        program = b"#!/bin/sh\nprintf 'fetchloom 0.1.0\\n'\n"
        dist = root / 'dist'
        dist.mkdir()
        write_tar(dist / archive_name(target, VERSION), [(f'fetchloom-{VERSION}-{target}/fetchloom', program)])
        (root / 'built').write_bytes(program)
        return dist, root / 'built'

    def shell_target(self):
        """this machine's linux release, or a skip where the shell installer cannot run."""
        arch = {'x86_64': 'x86_64', 'amd64': 'x86_64', 'aarch64': 'aarch64', 'arm64': 'aarch64'}.get(platform.machine())
        if arch is None or not (shutil.which('curl') or shutil.which('wget')) or not (shutil.which('sha256sum') or shutil.which('shasum')):
            self.skipTest('needs a released architecture, curl or wget and a SHA-256 command')
        return f'{arch}-unknown-linux-musl'

    def test_the_broken_archive_holds_only_a_program_that_cannot_run(self):
        for target in ['x86_64-pc-windows-msvc', 'x86_64-unknown-linux-musl']:
            with self.subTest(target):
                body = check_installer.invalid_archive(target, VERSION)
                folder = f'fetchloom-{VERSION}-{target}'
                if 'windows' in target:
                    with zipfile.ZipFile(io.BytesIO(body)) as archive:
                        self.assertEqual({name: archive.read(name) for name in archive.namelist()},
                                         {f'{folder}/fetchloom.exe': b'not an executable\n'})
                else:
                    with tarfile.open(fileobj=io.BytesIO(body)) as archive:
                        member, = archive.getmembers()
                        self.assertEqual((member.name, member.mode, archive.extractfile(member).read()),
                                         (f'{folder}/fetchloom', 0o755, b'not an executable\n'))

    def test_an_installer_that_runs_too_long_is_stopped_with_what_it_said(self):
        target = 'x86_64-unknown-linux-musl'
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dist, binary = self.release(root, target)
            args = argparse.Namespace(target=target, version=VERSION, commit=COMMIT, binary=binary, dist=dist, shell='sh',
                                      proofs=root / 'proofs')
            # posix keeps what a timed-out run said as bytes, whatever text= asked for
            for stdout, stderr, said in [(b'downloading \xff', 'checking', 'downloading \ufffdchecking'), (None, None, '')]:
                stopped = subprocess.TimeoutExpired(['sh', 'install.sh'], 90, output=stdout, stderr=stderr)
                with self.subTest(said=said), patch.object(check_installer, 'user_path', contextlib.nullcontext), \
                     patch.object(check_installer.subprocess, 'run', side_effect=stopped), self.assertRaises(RuntimeError) as raised:
                    check_installer.check(args)
                self.assertEqual(str(raised.exception), 'the installer ran past 90 s:\n' + said)

    @unittest.skipUnless(sys.platform.startswith('linux'), 'the shell installer runs on linux here, and on macos in artifacts.yml')
    def test_the_shell_installer_installs_refuses_and_updates_path_as_promised(self):
        target = self.shell_target()
        if shutil.which('fl'):
            self.skipTest('another fl on PATH keeps the installer from making its own')
        journeys = []

        class Checked:
            def __init__(self, installed, root, server):
                journeys.append(installed)

            def check(self, version, commit):
                return {'ok': True, 'version': version, 'commit': commit}

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dist, binary = self.release(root, target)
            arguments = ['check_installer.py', '--target', target, '--version', VERSION, '--commit', COMMIT, '--binary', str(binary),
                         '--dist', str(dist), '--shell', 'sh', '--proofs', str(root / 'proofs')]
            printed = io.StringIO()
            with patch.object(check_installer, 'Journey', Checked), patch.object(sys, 'argv', arguments), \
                 patch.dict(os.environ, {'RUNNER_TEMP': str(root)}), contextlib.redirect_stdout(printed):
                check_installer.main()
            proved = {'ok': True, 'version': VERSION, 'commit': COMMIT, 'shell': 'sh'}
            self.assertEqual(json.loads((root / 'proofs/installer-sh.json').read_text(encoding='utf-8')), proved)
        self.assertEqual(printed.getvalue(), json.dumps(proved) + '\n')
        # the journey ran the program the installer put in place
        installed, = journeys
        self.assertEqual(installed.parts[-4:], ('home with spaces', '.local', 'bin', 'fetchloom'))

    @unittest.skipUnless(sys.platform.startswith('linux'), 'the shell installer runs on linux here, and on macos in artifacts.yml')
    def test_an_installer_that_breaks_a_promise_is_caught(self):
        target = self.shell_target()
        # the real installer, then one fault
        wrapper = (f'sh {shlex.quote(str(check_installer.ROOT / "install.sh"))} "$@"\n'
                   'code=$?\n'
                   'case $INSTALLER_FAULT in\n'
                   '    succeeds) exit 0 ;;\n'
                   '    replaces) [ "$code" = 0 ] || printf other > "$HOME/.local/bin/fetchloom" ;;\n'
                   '    touches) [ "${FETCHLOOM_NO_MODIFY_PATH:-}" != 1 ] || printf \'# touched\\n\' >> "$HOME/.profile" ;;\n'
                   '    twice) [ -n "${FETCHLOOM_NO_MODIFY_PATH:-}" ] || [ "$code" != 0 ] ||\n'
                   '        printf \'export PATH="$HOME/.local/bin:$PATH"\\n\' >> "$HOME/.profile" ;;\n'
                   'esac\n'
                   'exit $code\n')

        class Checked:
            def __init__(self, installed, root, server):
                pass

            def check(self, version, commit):
                return {'ok': True}

        for fault, said in [('succeeds', 'nothing was installed'), ('replaces', 'failed install replaced the installation'),
                            ('touches', 'no-modify-path changed a profile'), ('twice', 'profile update was not idempotent')]:
            with self.subTest(fault), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                dist, binary = self.release(root, target)
                (root / 'install.sh').write_text(wrapper, encoding='utf-8')
                args = argparse.Namespace(target=target, version=VERSION, commit=COMMIT, binary=binary, dist=dist, shell='sh',
                                          proofs=root / 'proofs')
                with patch.object(check_installer, 'ROOT', root), patch.object(check_installer, 'Journey', Checked), \
                     patch.dict(os.environ, {'RUNNER_TEMP': str(root), 'INSTALLER_FAULT': fault}), \
                     self.assertRaisesRegex(AssertionError, said):
                    check_installer.check(args)


class WheelCheckTests(unittest.TestCase):
    """check_wheel.py with venv, pip, the journey and the helper tests standing in."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name).resolve()
        (self.root / 'python').mkdir()
        for name in ['test_helper.py', 'wheel_scripts.py']:
            (self.root / 'python' / name).write_text('', encoding='utf-8')
        self.binary = self.root / 'fetchloom'
        self.binary.write_bytes(b'checked binary')
        self.wheel = self.root / wheel_name('x86_64-unknown-linux-musl')
        write_wheel(self.wheel, self.binary.read_bytes())
        self.windows = sys.platform == 'win32'
        self.prefix = self.root / '.release/wheel-check/wheel-current'
        self.scripts = self.prefix / ('Scripts' if self.windows else 'bin')
        self.python = self.scripts / ('python.exe' if self.windows else 'python')
        self.installed = self.binary.read_bytes()
        self.steps = []

    def stand_in(self, command, **options):
        words = [str(word) for word in command]
        step = {'command': words}
        if words[1:3] == ['-m', 'venv']:
            self.scripts.mkdir(parents=True)
        elif words[1:3] == ['-m', 'pip']:
            (self.scripts / ('fetchloom.exe' if self.windows else 'fetchloom')).write_bytes(self.installed)
        else:
            # a fetchloom found on PATH first would answer with exit code 86
            decoy = Path(options['env']['PATH'].split(os.pathsep)[0]) / ('fetchloom.exe' if self.windows else 'fetchloom')
            step['decoy'] = decoy.read_text(encoding='utf-8')
            if words[2].endswith('ci_journey.py'):
                report = Path(words[words.index('--report') + 1])
                report.write_text(json.dumps({'ok': True, 'version': VERSION, 'commit': COMMIT, 'helper': True}), encoding='utf-8')
            else:
                step['tests'] = sorted(path.name for path in Path(words[words.index('-s') + 1]).iterdir())
        self.steps.append(step)
        return subprocess.CompletedProcess(command, 0)

    def check(self):
        arguments = ['check_wheel.py', str(self.wheel), '--binary', str(self.binary), '--version', VERSION, '--commit', COMMIT,
                     '--stage', 'wheel-current', '--proofs', str(self.root / 'proofs')]
        (self.root / 'proofs').mkdir(exist_ok=True)
        with patch.object(check_wheel, 'ROOT', self.root), patch.object(sys, 'argv', arguments), \
             patch.object(check_wheel.subprocess, 'run', side_effect=self.stand_in), \
             patch.object(check_wheel, 'check_version') as versions:
            check_wheel.main()
        return versions

    def test_the_installed_wheel_runs_its_launchers_journey_and_helper_tests(self):
        versions = self.check()
        self.assertEqual(json.loads((self.root / 'proofs/wheel-current.json').read_text(encoding='utf-8')),
                         {'ok': True, 'version': VERSION, 'commit': COMMIT, 'helper': True,
                          'python': sys.version.split()[0], 'wheel': self.wheel.name})
        venv, pip, journey, tests = self.steps
        self.assertEqual(venv['command'], [sys.executable, '-m', 'venv', str(self.prefix)])
        self.assertEqual(pip['command'], [str(self.python), '-m', 'pip', 'install', '--disable-pip-version-check',
                                          '--no-index', '--no-deps', str(self.wheel)])
        program = self.scripts / ('fetchloom.exe' if self.windows else 'fetchloom')
        self.assertEqual(journey['command'][:5], [str(self.python), '-I', str(self.root / 'python/ci_journey.py'), '--binary', str(program)])
        self.assertIn('--helper', journey['command'])
        self.assertEqual(tests['command'][:5], [str(self.python), '-I', '-m', 'unittest', 'discover'])
        self.assertEqual(tests['tests'], ['global-bin', 'home', 'test_helper.py', 'wheel_scripts.py'])
        self.assertEqual({journey['decoy'], tests['decoy']}, {'#!/bin/sh\nexit 86\n'})
        (fl, *_), (module, *_) = (call.args for call in versions.call_args_list)
        self.assertEqual((fl, module), (self.scripts / ('fl.exe' if self.windows else 'fl'), [self.python, '-I', '-m', 'fetchloom']))

    def test_another_installed_executable_or_a_used_environment_is_refused(self):
        self.installed = b'another binary'
        with self.assertRaisesRegex(AssertionError, 'installed executable differs'):
            self.check()
        with self.assertRaisesRegex(AssertionError, 'wheel check requires a fresh environment'):
            self.check()

    def test_restored_rust_cache_does_not_supply_the_wheel_environment(self):
        class EnvironmentSelected(Exception):
            pass

        cached = self.root / 'target/wheel-check/wheel-current'
        cached.mkdir(parents=True)
        (cached / 'old-install').write_text('previous revision', encoding='utf-8')
        arguments = ['check_wheel.py', 'package.whl', '--binary', 'fetchloom', '--version', VERSION, '--commit', COMMIT,
                     '--stage', 'wheel-current', '--proofs', str(self.root / 'proofs')]
        with patch.object(check_wheel, 'ROOT', self.root), patch('sys.argv', arguments), \
             patch.object(check_wheel, 'inspect_wheel'), \
             patch.object(check_wheel.subprocess, 'run', side_effect=EnvironmentSelected) as run:
            with self.assertRaises(EnvironmentSelected):
                check_wheel.main()
        prefix = Path(run.call_args.args[0][-1])
        self.assertFalse(prefix.is_relative_to(self.root / 'target'))
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
