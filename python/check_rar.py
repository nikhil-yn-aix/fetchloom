"""execute the extraction command the checked executable actually prints."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

from ci_journey import ROOT, Journey, Server, digest, require


def check(binary, version, commit):
    fixture = ROOT / 'tests/fixtures/archives/libarchive-solid.rar'
    expected = json.loads(fixture.with_suffix('.json').read_text(encoding='utf-8'))
    cases = []
    with tempfile.TemporaryDirectory(prefix='fetchloom-rar-', dir=os.environ.get('RUNNER_TEMP')) as folder:
        for number, name in enumerate(['sample.rar', "my data's.rar"]):
            root = Path(folder) / str(number)
            root.mkdir()
            with Server({'/' + name: fixture.read_bytes()}) as server:
                journey = Journey(binary, root, server)
                project = journey.project('project')
                result = journey.run(project, 'add', server.url('/' + name), '--as', 'sample')
                warnings = result['warnings']
                require(len(warnings) == 1, f'expected one rar warning: {warnings}')
                commands = re.findall(r'`([^`]+)`', warnings[0])
                if sys.platform == 'win32':
                    commands = [command for command in commands if command.startswith('& ') and '7z.exe' in command]
                    prefix = "$ErrorActionPreference = 'Stop'\n"
                    suffix = '\nif ($LASTEXITCODE) { throw "rar extraction failed" }\n'
                    script = project / 'extract.ps1'
                    invocation = ['pwsh', '-NoProfile', '-NonInteractive', '-File', str(script)]
                else:
                    reader = 'unar' if sys.platform == 'darwin' else 'unrar'
                    commands = [command for command in commands if command.startswith(reader + ' ')]
                    prefix, suffix = 'set -eu\n', '\n'
                    script = project / 'extract.sh'
                    invocation = ['sh', str(script)]
                require(len(commands) == 1, f'no unique extraction command: {warnings}')
                script.write_text(prefix + commands[0] + suffix, encoding='utf-8')
                completed = subprocess.run(invocation, cwd=project, env=journey.env,
                                           capture_output=True, text=True, timeout=60)
                require(completed.returncode == 0, completed.stdout + completed.stderr)
                unpacked = project / (Path(name).stem + '-unpacked')
                actual = {path.relative_to(unpacked).as_posix(): digest(path)
                          for path in unpacked.rglob('*') if path.is_file()}
                require(actual == expected, f'printed command unpacked different bytes or layout: {actual}')
                print('executed:', commands[0])
                cases.append({'command': commands[0], 'files': actual})
    return {'ok': True, 'version': version, 'commit': commit, 'binary_sha256': digest(binary), 'cases': cases}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--proofs', type=Path, required=True)
    args = parser.parse_args()
    result = check(args.binary.resolve(), args.version, args.commit)
    args.proofs.mkdir(parents=True, exist_ok=True)
    (args.proofs / 'rar.json').write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
