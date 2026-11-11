"""install a wheel into a clean interpreter and exercise its real helper and binary."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import zipfile
from email.parser import Parser

from ci_journey import ROOT, digest, environment, require


def inspect_wheel(wheel, version, binary):
    with zipfile.ZipFile(wheel) as packed:
        metadata = [name for name in packed.namelist() if name.endswith('.dist-info/METADATA')]
        require(len(metadata) == 1, "wheel has no unique metadata")
        fields = Parser().parsestr(packed.read(metadata[0]).decode())
        require(fields['Name'] == 'fetchloom' and fields['Version'] == version, "wrong wheel identity")
        require(fields['Requires-Python'] == '>=3.10', "wheel changed the Python floor")
        require('fetchloom/py.typed' in packed.namelist() and 'fetchloom/__init__.py' in packed.namelist(), "wheel omitted its helper")
        programs = [name for name in packed.namelist() if name.endswith('/scripts/' + binary.name)]
        require(len(programs) == 1, "wheel has no unique executable")
        require(hashlib.sha256(packed.read(programs[0])).hexdigest() == digest(binary), "wheel rebuilt or omitted the checked executable")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('wheel', type=Path)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--stage', choices=['wheel-current', 'wheel-minimum'], required=True)
    parser.add_argument('--proofs', type=Path, required=True)
    args = parser.parse_args()
    inspect_wheel(args.wheel, args.version, args.binary)
    prefix = (ROOT / 'target/wheel-check' / args.stage).resolve()
    require(not prefix.exists(), 'wheel check requires a fresh environment')
    subprocess.run([sys.executable, '-m', 'venv', str(prefix)], check=True)
    scripts = prefix / ('Scripts' if sys.platform == 'win32' else 'bin')
    python = scripts / ('python.exe' if sys.platform == 'win32' else 'python')
    subprocess.run([str(python), '-m', 'pip', 'install', '--disable-pip-version-check', '--no-index', '--no-deps', str(args.wheel.resolve())], check=True)
    program = scripts / ('fetchloom.exe' if sys.platform == 'win32' else 'fetchloom')
    require(digest(program) == digest(args.binary), 'installed executable differs')
    report = args.proofs.resolve() / f'{args.stage}.json'
    with tempfile.TemporaryDirectory(prefix='fetchloom-wheel-tests-', dir=os.environ.get('RUNNER_TEMP')) as directory:
        tests = Path(directory)
        home = tests / 'home'
        home.mkdir()
        decoy = tests / 'global-bin'
        decoy.mkdir()
        wrong = decoy / program.name
        wrong.write_text('#!/bin/sh\nexit 86\n', encoding='utf-8')
        wrong.chmod(0o755)
        env = environment(home)
        env['PATH'] = str(decoy) + os.pathsep + env['PATH']
        subprocess.run([str(python), '-I', str(ROOT / 'python/ci_journey.py'), '--binary', str(program),
                        '--helper', '--version', args.version, '--commit', args.commit, '--report', str(report)],
                       cwd=tests, env=env, check=True)
        shutil.copyfile(ROOT / 'python/test_helper.py', tests / 'test_helper.py')
        subprocess.run([str(python), '-I', '-m', 'unittest', 'discover', '-s', str(tests), '-p', 'test_helper.py'],
                       cwd=tests, env=env, check=True)
    document = json.loads(report.read_text(encoding='utf-8'))
    document['python'] = sys.version.split()[0]
    document['wheel'] = args.wheel.name
    report.write_text(json.dumps(document, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
